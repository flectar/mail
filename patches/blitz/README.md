# Blitz patch maintenance

The vendored crates are the published **0.3.0-beta.4** archives at revision
`92b11f1bd8ab7975411328b0406a95a09445b4e7`, plus the ordered patches in
[`series`](series). Archive SHA-256 checksums are in [`upstream.toml`](upstream.toml).
Do not edit the vendored code without regenerating its patch.

```sh
bash scripts/blitz-patches.sh verify
bash scripts/blitz-patches.sh stage NEW_VERSION
```

Verification downloads and checks both archives, checks their source revision,
applies every patch without rejects, and compares every file with the vendored
trees. It also checks the application/probe pins, float feature,
desktop/Android/OpenHarmony patch wiring, and all three resolved lockfiles.

## Patches

| Patch | Purpose |
| --- | --- |
| 0002 reader resources and images | Decode limits with native JPEG scaling (CMYK included), progressive-JPEG and allocation guards, premultiplied downscaling and EXIF orientation, separate intrinsic/pixel sizes, paint-only updates for images whose layout cannot change, resource notifications, selection paragraph separators and range checks. |
| 0003 email tables and presentational hints | Table collector and automatic sizing (content minimums, authored widths kept while they fit, proportional authored columns, wrapping), rowspan handling, collapsed edges, cached legacy hints, visibility export and clip-aware hit testing. |
| 0004 inline floats | Floats-disabled line breaking, bounded float width, the float's own margin basis, a non-looping height-limit failure, float line slots clamped to the layout width, and floats that move with `align-content` (cell `vertical-align`). |
| 0005 email painting and cache | Per-edge collapsed table borders, inline ancestor backgrounds, fixed-image paint invalidation, and a bounded line cache with conservative culling. |
| 0006 repository integration | objc2 dependency-scanner exception, removal of paint's unused `euclid` and `usvg`, and the bounded JPEG decoder dependency. |
| 0007 unresolvable resource URLs | Skips images, stylesheets, form actions and `@import` rules whose URL cannot be resolved instead of panicking. The reader has no base URL, so a relative `<img src>` failed the whole message. |
| 0008 offscreen culling bounds | Builds each element's cull box at its own origin, like its scaled scrollable overflow. Tiles far down a message no longer paint the elements above them. |

0002–0006 are not upstream yet, except 0004's float slot clamp, which backports
upstream #1443, and 0003's table rebuild on border, padding and size changes,
which backports #1276's `table_needs_rebuild`. Upstream main adds `cellspacing` and
`cellpadding` hints (#1366), which overlap 0003. 0007 backports upstream #1374
and the `<link>` part of #1318; its `@import` change is local. 0008 backports
upstream #1342, keeping beta.4's element transform. Drop the backported parts
when a release contains them.

Table-cell layout adjustments use temporary computed-style overrides. They do
not mutate the author's primary style or its shared hint blocks. Each call
restores the previous override; table reconstruction refreshes cached styles.

Beta.3 removed the implicit available-width clamp on replaced elements. The
reader supplies `img { max-width: 100% }` in its user-agent stylesheet to
preserve fitted images. Sender CSS can override that default.

Parley 0.12's published `parley_engine` includes the emoji-presentation state
machine previously backported locally. Slint still depends on Parley 0.11.1, so
its existing backport and verifier remain in place until Slint adopts a fixed
version too.

## Beta.4 rebase

Beta.4 keeps Stylo 0.22, cssparser 0.38, html5ever 0.40, Taffy 0.15,
Parley/Fontique 0.12 and AnyRender 0.14. All six patches were retained; only
their context changed:

- `DocumentConfig` has a hand-written `Default`, which 0002 extends with
  `image_decode_limits: None`.
- The `std::sync` import and the keyboard-types 0.8 manifest line moved.
- blitz-paint no longer names `usvg`, so 0006 drops it with `euclid`. blitz-dom
  still enables it with the same features.

User-agent stylesheets are now parsed `UserAgentStylesheet` values. The reader
passes Blitz's default sheet and its own, each parsed once, so its documents
also share Stylo's user-agent cascade data. Keys use keyboard-types 0.8
(`Key::Named`).

## Upgrade validation

```sh
cargo test -p flectar-renderer-probe --test blitz_upgrade --locked
cargo check --workspace --all-targets --locked
cargo check -p flectar-mail --no-default-features --locked
cargo check -p flectar-mail --no-default-features --features remote-content --locked
cargo check -p flectar-mail --features flatpak --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --all-targets --locked
cargo test -p blitz-dom --lib --locked
cargo metadata --manifest-path platform/android/Cargo.toml --locked --format-version 1 > /dev/null
cargo metadata --manifest-path platform/ohos/Cargo.toml --locked --format-version 1 > /dev/null
# With the NDK compiler environment used by scripts/build-android-test-apk.sh:
cargo check --manifest-path platform/android/Cargo.toml --target aarch64-linux-android --locked
# Requires a working WGPU adapter:
cargo test -p flectar-mail --lib issue_23_gpu --locked -- --ignored
```

`crates/renderer-probe/tests/blitz_upgrade.rs` covers the upstream/local table
overlap and locale-sensitive text. Application renderer tests cover float
wrapping, selection, image invalidation, caching, backgrounds, and emoji.
Refresh Flatpak's `cargo-sources.json` whenever the root lockfile changes.

On Linux, the beta.4 queue passed the Blitz library suite (88 tests), the
focused upgrade suite (nine tests), patch reproduction, all three locked
metadata checks and the Flatpak source check. The application crate is
validated by CI.
