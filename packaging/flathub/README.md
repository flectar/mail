# Flathub packaging

`com.flectar.mail.yml` is the manifest Flathub builds. It matches the bundle
manifest in `packaging/flatpak` with two differences, and
`scripts/test-release.py` fails if anything else drifts between them:

- It builds a release tag from GitHub instead of the local checkout.
- It leaves out the GitHub update check, because Flathub updates the app.

Both build PDFium from source with `packaging/flatpak/pdfium.yml`, because
Flathub only accepts software built from source. The module pins the same
PDFium branch that `scripts/stage-pdfium.py` downloads for the other packages,
and the app's PDF preview test runs against it during the build.

The `flathub` job in `.github/workflows/native-platforms.yml` builds the
checkout with Flathub's own builder (`flathub-build` from
`org.flatpak.Builder`) and runs `flatpak-builder-lint` on the manifest and on
the built repository, so a pull request finds Flathub's errors before a
release does.

## Updating PDFium

Pick the new branch in `scripts/stage-pdfium.py`, then take every revision in
`pdfium.yml` from that branch's `DEPS` file: `build_revision`,
`abseil_revision`, `simdutf_revision`, `dragonbox_revision`,
`fast_float_revision` and `gn_version`. The shim header script comes from the
matching `refs/branch-heads/<branch>` of Chromium.

## Updating Flathub after a release

Flathub keeps the manifest in its `flathub/com.flectar.mail` repository. Fill
in the tag and its full commit hash, then copy the output there:

```bash
python3 scripts/flathub-manifest.py \
  --tag v0.1.0 --commit "$(git rev-list -n 1 v0.1.0)" target/flathub/release
```

The output directory holds `com.flectar.mail.yml`, `pdfium.yml` and a copy of
`packaging/flatpak/cargo-sources.json`, which every lockfile change refreshes.
