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
| 0002 reader resources and images | Decode limits with native JPEG scaling (CMYK included), progressive-JPEG and allocation guards, premultiplied downscaling and EXIF orientation, separate intrinsic/pixel sizes, paint-only updates for images whose layout cannot change, resource notifications, selection text separated like `innerText` (tabs between cells, blank lines around paragraphs) with range checks. |
| 0003 email tables and presentational hints | Table collector and automatic sizing (content minimums, authored widths interpolated toward content as in CSS Tables 3, proportional authored columns, wrapping), fixed tables as wide as their columns, rowspan handling, collapsed edges, cached legacy hints and clip-aware hit testing. |
| 0004 inline floats | Floats-disabled line breaking, bounded float width, the float's own margin basis, no line height limit (so no height-limit failure), float line slots clamped to the layout width, and floats that move with `align-content` (cell `vertical-align`). |
| 0005 email painting and cache | Per-edge collapsed table borders, inline ancestor backgrounds, fixed-image paint invalidation, a bounded line cache with conservative culling, and culling that keeps shadows and other ink near tile edges. |
| 0006 repository integration | objc2 dependency-scanner exception, removal of paint's unused `euclid` and `usvg`, and the bounded JPEG decoder dependency. |
| 0007 unresolvable resource URLs | Skips images, stylesheets, form actions and `@import` rules whose URL cannot be resolved instead of panicking. The reader has no base URL, so a relative `<img src>` failed the whole message. |
| 0008 offscreen culling bounds | Builds each element's cull box at its own origin, like its scaled scrollable overflow, and rescales every element's overflow after a scale change. Tiles far down a message no longer paint the elements above them, nor cull content whose overflow was measured at another scale. |
| 0009 inside list markers before blocks | Keeps a `list-style-position: inside` marker in front of the list item's content when that content starts with a block or is empty, on an anonymous line of its own as in browsers. A `<summary>` that holds a heading, as GitHub's release mails have, lost its disclosure triangle. |
| 0010 email table sizing | A cell's `height` is the minimum of its row (CSS 2.1 §17.5.3): the cell is measured with it as `min-height`, so its content no longer spills into the next row. Consecutive children of a row that aren't cells share one anonymous cell (§17.2.1), where responsive emails' `display: block` cells stack instead of overflowing side by side. Auto columns share the width left over after their min-content widths in proportion to how far each can grow toward its max-content width, as Chromium does (CSS Tables 3 §3.9.3), where grid tracks grew by equal amounts. |
| 0011 inline boxes and text painting | A non-atomic inline element's inline-axis margin, border and padding take room in its line (CSS 2.1 §10.3.1), each edge laid out as an invisible placeholder (U+2061 at the start, U+2060 at the end) whose letter spacing is the edge's width. White space collapses across the placeholders and line breaks keep them with their content. Backgrounds (gradients and images included), borders, radii and box shadows paint on each line fragment, sliced from one strip of the element's fragments (`box-decoration-break: slice`), and `opacity` groups an element's fragments and text. Each line paints its boxes, then its selection, then its text. A `<wbr>` is a line break opportunity even in text that doesn't wrap, and `<br>` and `display: contents` content sits on its parent's baseline. Text decorations follow Blink: an `auto` thickness of a tenth of the font size, lengths rounded, an `auto` underline just below the baseline by half its thickness, lines on whole device pixels, ink skipped where glyphs cross underlines and overlines (not for `/`, `\`, `_` or CJK, as in Blink), and no decoration under white space that hangs at a line's end. A face without a bold is emboldened as Skia fakes bold. |
| 0012 table captions, anonymous tables and bevels | A table's captions are laid out as wide as the table, above its grid (below it with `caption-side: bottom`), and the table's background and border surround the grid alone (CSS 2.1 §17.4). They were dropped, text and all. Table parts outside a table, such as the rows of a `display: block` table or `display: table-cell` boxes without one, are wrapped in an anonymous table (§17.2.1, through a `::-servo-anonymous-table` user-agent rule) instead of laying out side by side as flex items. Inset and outset borders shade as Blink's `CalculateInsetOutsetColor` does, so black bevels between #545454 and #a8a8a8. |
| 0013 image minimums and Chromium line breaks | A `min-width` or `min-height` no longer changes an image's specified size on the other axis (sections 10.3.2 and 10.6.2 of CSS 2.1), so an email spacer with `height: 1px; min-width: 540px` is 540 by 1 pixels instead of a 540 pixel square gap. Maximums still scale both sizes, unlike in CSS 2.1, so images the reader fits with `max-width: 100%` keep their proportions. Lines break where Chromium breaks them, between two ASCII characters only where its table allows and after every run of spaces, so a path stays whole and a date may break after a hyphen. The table looks through 0011's edge placeholders, and text with `word-break: break-all` or `line-break: anywhere` keeps the breaks of UAX #14. |

0002–0006 are not upstream yet, except 0004's float slot clamp and shrink-to-fit
float widths, which backport upstream #1443 and #980, and 0003's table rebuild on border, padding and size changes,
which backports #1276's `table_needs_rebuild`, and 0005's clamped `<img>` edges, which
backport open upstream #1201. Upstream main adds `cellspacing` and
`cellpadding` hints (#1366), which overlap 0003. 0007 backports upstream #1374
and the `<link>` part of #1318; its `@import` change is local. 0008 backports
upstream #1342, keeping beta.4's element transform, and open upstream #1372. 0009 to 0013 are local. Drop the backported parts
when a release contains them.

Copied text, find in the reader and accessible names leave out 0011's edge
placeholders (`blitz_dom::node::without_inline_edges`).

Table-cell layout adjustments use temporary computed-style overrides. They do
not mutate the author's primary style or its shared hint blocks. Each call
restores the previous override, and a cell's cached override is rebuilt
whenever its primary style changes.

Beta.3 removed the implicit available-width clamp on replaced elements. The
reader supplies `img { max-width: 100% }` in its user-agent stylesheet to
preserve fitted images. Sender CSS can override that default.

Parley 0.12's published `parley_engine` includes the emoji-presentation state
machine previously backported locally. Slint 1.19 uses Parley 0.12 too, so no
Parley backport remains.

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
overlap, locale-sensitive text, 0010's and 0011's table sizing, inline
boxes, opacity, decorations and line breaks, 0012's captions, anonymous
tables and bevels, and 0013's image minimums and line breaks. Application
renderer tests cover float
wrapping, selection, image invalidation, caching, backgrounds, and emoji.
Refresh Flatpak's `cargo-sources.json` whenever the root lockfile changes.

On Linux, the beta.4 queue passed the Blitz library suite (88 tests), the
focused upgrade suite (nine tests), patch reproduction, all three locked
metadata checks and the Flatpak source check. The application crate is
validated by CI.
