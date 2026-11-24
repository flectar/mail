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
| 0010 email table sizing | A cell's `height` is the minimum of its row (section 17.5.3 of CSS 2.1). The cell is measured with it as `min-height`, so its content no longer spills into the next row. Consecutive children of a row that aren't cells share one anonymous cell (section 17.2.1), where responsive emails' `display: block` cells stack instead of overflowing side by side. Auto columns share the width left over after their min-content widths in proportion to how far each can grow toward its max-content width, as Chromium does (section 3.9.3 of CSS Tables 3), where grid tracks grew by equal amounts. |
| 0011 inline boxes and text painting | A non-atomic inline element's inline-axis margin, border and padding take room in its line (section 10.3.1 of CSS 2.1), each edge laid out as an invisible placeholder (U+2061 at the start, U+2060 at the end) whose letter spacing is the edge's width. White space collapses across the placeholders and line breaks keep them with their content. Backgrounds (gradients and images included), borders, radii and box shadows paint on each line fragment, sliced from one strip of the element's fragments (`box-decoration-break: slice`), and `opacity` groups an element's fragments and text. Each line paints its boxes, then its selection, then its text. A `<wbr>` is a line break opportunity even in text that doesn't wrap, and `<br>` and `display: contents` content sits on its parent's baseline. Text decorations follow Blink, with an `auto` thickness of a tenth of the font size, lengths rounded, an `auto` underline just below the baseline by half its thickness, lines on whole device pixels, ink skipped where glyphs cross underlines and overlines (not for `/`, `\`, `_` or CJK, as in Blink), and no decoration under white space that hangs at a line's end. A face without a bold is emboldened as Skia fakes bold. |
| 0012 table captions, anonymous tables and bevels | A table's captions are laid out as wide as the table, above its grid (below it with `caption-side: bottom`), and the table's background and border surround the grid alone (section 17.4 of CSS 2.1). They were dropped, text and all. Table parts outside a table, such as the rows of a `display: block` table or `display: table-cell` boxes without one, are wrapped in an anonymous table (section 17.2.1, through a `::-servo-anonymous-table` user-agent rule) instead of laying out side by side as flex items. Inset and outset borders shade as Blink's `CalculateInsetOutsetColor` does, so black bevels between #545454 and #a8a8a8. |
| 0013 image minimums and Chromium line breaks | A `min-width` or `min-height` no longer changes an image's specified size on the other axis (sections 10.3.2 and 10.6.2 of CSS 2.1), so an email spacer with `height: 1px; min-width: 540px` is 540 by 1 pixels instead of a 540 pixel square gap. Maximums still scale both sizes, unlike in CSS 2.1, so images the reader fits with `max-width: 100%` keep their proportions. Lines break where Chromium breaks them, between two ASCII characters only where its table allows and after every run of spaces, so a path stays whole and a date may break after a hyphen. The table looks through 0011's edge placeholders, and text with `word-break: break-all` or `line-break: anywhere` keeps the breaks of UAX #14. |
| 0014 table border attributes and spare width | A table's `border`, `frame`, `rules` and `bordercolor` attributes set its borders as Blink does, through presentational hints instead of Gecko's user-agent rules, whose `frame` sides the `border` hint used to override with `outset`. `frame` draws the sides it names solid, `bordercolor` makes the table's and its cells' borders solid in that colour instead of bevels, `rules` picks the borders between cells, and cells take the table's border colour through their rows and row groups, as the HTML rendering rules and Blink have them. A table with an authored width wider than its auto columns' max-content widths shares the excess in proportion to those widths, so an empty column gets none of it (section 3.9.3 of CSS Tables 3), where grid tracks grew by equal amounts. A table without columns has no border spacing. |
| 0015 selection line breaks and text highlights | A line whose end the selection goes past ends in a box as wide as a space in its last run's font, as in Blink, which shows that its line break is selected. That covers lines ending in a `<br>` (where Parley drew a narrower box), words wrapped within themselves, and the end of a paragraph that the selection continues beyond, which `get_text_selection_ranges_and_end` tells apart. `set_text_highlights` highlights parts of the text as find in page does, painted as Blink paints its matches, yellow and the active one orange, as tall as the text and over the selection, with the text in black. |
| 0016 cell percentage padding and legacy word-break | A table cell's percentage padding resolves against the table's content width, as in Blink, rather than against its own column, so MJML's hero cell with `padding-bottom: 70%` is as tall as 70% of the table instead of nearly nothing. An auto-width table is laid out again at the width it took when any cell has percentage padding. Author CSS has its legacy `word-break: break-word`, which Stylo drops outside Gecko, rewritten to the `word-break: normal` and `overflow-wrap: anywhere` it stands for (section 5.2 of CSS Text 3), so long words in narrow columns break as they do in browsers. |
| 0017 unavailable images | An `<img>` that won't load lays out with its alt text, as browsers lay out an image they can't show, on one line or wrapped inside a width or max-width it is given, in its own font. That covers images that fail to decode and ones the embedder fails through `BaseDocument::fail_image`, with `pending_image_urls` listing the images still waiting. An empty alt takes no room, and set sizes still apply. |
| 0018 rules and bevelled borders | An `<hr>`'s `color`, `noshade` and `size` attributes map as Blink maps them, through presentational hints instead of Gecko's `hr[size="1"]` user-agent rule. `color` and `noshade` make a solid rule filled with the colour, or with Blink's dark gray (#808080) for `noshade`, and `size` sets the rule's height, or drops its bottom border at 1 or less. An `inset`, `outset`, `groove` or `ridge` border in `currentcolor` is shaded from rgb(238, 238, 238) instead of the text colour, as Blink's `BorderSideColor` does for anything but tables and their parts, so a default `<hr>` is as pale as in browsers. |
| 0019 text shadows | `text-shadow` paints as section 4 of CSS Text Decoration 3 describes, the text drawn again under itself, offset and in the shadow's colour, the first shadow on top, with the text's decorations shadowed too. A line paints all its shadows before its text, so no shadow covers the line's text. A blurred shadow is drawn in a layer with a Gaussian blur filter whose standard deviation is half the blur radius, and a renderer without filters draws it unblurred. Lines with shadows are never culled, and runs are culled by where their shadows reach. |
| 0020 selection into text after a block | Collects the inline roots between the ends of a text selection from the layout tree, which holds the anonymous blocks that wrap text next to a block. A selection from a paragraph into the text after it ran backwards and selected nothing, so it was neither highlighted nor copied. |
| 0021 list numbering and markers | Lists are numbered as the HTML standard numbers them (section 4.4.8). An `<ol>` counts from its `start`, or when `reversed` down from its number of items, and an `<li>`'s `value` sets its own number, which the count goes on from. `reversed` never applied before, and `start="0"` overflowed. The `type` attributes of `<ol>`, `<ul>` and `<li>` map to `list-style-type` as Blink maps them. Markers come in the predefined counter styles of CSS Counter Styles 3, decimal, decimal-leading-zero, lower and upper roman, alpha and latin, and lower-greek, falling back to decimal outside a style's range and for the styles Blitz doesn't define, which drew a white square. An outside marker keeps the space after its `.`, as `::marker` has `white-space: pre`, so `1.` ends a space before the item's text as in browsers. |
| 0022 background and clear attributes | The `background` attribute of `<body>`, a table or a table part is a background image (HTML rendering rules), resolved against the document's URL, which presentational hints now resolve their URLs against. `<br clear>` starts the line after the `<br>` below the floats it clears, as in browsers, through an empty out-of-flow box that the line breaker stops at, and the block grows to hold the lines it moves, which Parley's sum of line heights missed. A `<br clear>` at the end of a block adds only the clearance. |
| 0023 soft hyphens | A line that breaks at a soft hyphen (U+00AD, `&shy;`) ends with a hyphen, as CSS Text 3 says and browsers show, U+2010 where the font has one, as in Blink, or else a hyphen-minus, in the font, colour and synthesis of the line's last text. Parley shapes the soft hyphen itself as nothing and gives Blitz no way to keep the hyphen's width in the line, so in right-aligned or centred text the hyphen hangs past the alignment edge. |
| 0024 images without a source | An `<img>` whose `src` is missing, empty or doesn't resolve, such as a relative URL in mail without a base URL, is broken, as the HTML standard's "update the image data" has it, and lays out with its alt text like 0017's images that won't load, where it took no room. |
| 0025 percentage columns and cell max-width | An auto-width table whose columns all have percentages is as wide as its widest column needs at its percentage, within its room, as Blink estimates it, and the columns share that width in proportion to their percentages, so a lone `width: 87.5%` column fills its table instead of leaving the rest empty. A cell always fills its column, as in Blink, where a length `max-width` only limits what the cell asks of the column and a percentage one is ignored, so a cell with `max-width: 33.333%` no longer shrinks to a third of its column. |
| 0026 blocks inside inline elements | An inline element that holds a block, such as a link around an image and its caption, is split around the block as CSS 2.1 section 9.2.1.1 has it and browsers show it. The block becomes a child of the container, between anonymous blocks that hold the inline content around it with the element's parts before and after the block, so the element's text before the block shares a line with the text before the element, and its text after the block shares one with what follows, such as a `<br>`, which made an empty line. The element's padding and border are on its first and last lines instead of around the block. One that holds only blocks still lays out as a block box, as before, which matches browsers and keeps a box for the reader's links. |
| 0027 table baselines | A table's first baseline is its first row's and its last baseline its last row's, as CSS 2.1 section 10.8.1 and Blink have it, which is where an inline table sits in its line. A row's baseline is the lowest first baseline of its cells aligned on their baseline that hold content, and without one, as with the `middle` cells of most tables, the bottom of the row less the least padding and border below a cell's content, as Blink's row layout finds it. Before, the first cell's first line set it whatever the cell's alignment, so a line with an inline table lacked its descent, and a caption above the table didn't lower it. A table's `valign` maps to its `vertical-align` as in Blink, so `valign="top"` sets an inline table at the top of its line. |
| 0028 table widths | A table is at least as wide as its captions' min-content widths with their margins, as in Blink, so a caption longer than the columns widens the table instead of breaking its words onto lines of their own. A fixed-layout table sizes its columns as Blink does (CSS 2.1 section 17.5.2.1). A percentage column takes its percentage and its cells' padding and border, and percentage columns scale down together when they overflow the table, where they ran past its edge. Length and percentage columns scale up to fill the table when no auto column takes the rest, auto columns share what is left, and length columns wider than the table widen it. An auto-width table narrower than its columns' minimums widens to them, as in browsers. Only where that would overflow its container do text-only cells break their long words, once the table has taken all the room it has, where before they broke whenever the columns overflowed the table's own width. |
| 0029 collapsed borders below captions | A table with `border-collapse: collapse` paints its cells' borders where its rows are below a caption above it, as the rows move down past the caption. They were painted where the rows would be without the caption, over it. |
| 0030 details and summary | A `<details>` lays out as its shadow tree has it in browsers (HTML "The details and summary elements"), its first `<summary>` first, wherever it is among the children, and everything else after it in one block while it is open. A closed one shows only its summary. Its own text was showing, since the UA rule that hides the rest with `display: none` can't reach text, and text before the summary came before it. |
| 0031 line box content | A line box holds content as CSS 2.1 section 9.4.2 counts it, as browsers do. One with nothing in it but white space that collapses away and empty inline elements, such as the space around an `<a name>` anchor, is a zero-height line box, so margins collapse through it, where it added a line's height. A block that holds only such content keeps its own width, `height` and `min-height`, as a spacer `<div style="height: 20px">` around an empty `<span>` does, where it took no room at all. Preserved white space is content, and so is an image, inline block or inline table without height, whose line keeps its strut, where it had none and a block holding a spacer image of no height had none either. |
| 0032 lang pseudo-class | `:lang()` matches an element by its language, its own `lang` or `xml:lang` or else its nearest ancestor's, with RFC 4647 extended filtering (Selectors 4 section 7.2), as Servo does, so `:lang(de)` matches `de-CH`. An unknown or empty language matches nothing, as in Blink and Gecko. It never matched. |
| 0033 superscript and subscript shifts | `vertical-align: super` raises a box by a third of its parent's font size and `sub` lowers it by a fifth, each plus a device pixel, as Blink and WebKit do. Parley leaves out the pixel, so a line that a superscript or subscript makes taller was a pixel shorter than in browsers. |
| 0034 shrink-to-fit widths | The width of a float, inline block, flex item or table column that fits its content keeps its fraction of a pixel, as in browsers, where Blitz rounded it up to a whole pixel, so a row of them drifted by up to a pixel per box. Content that just fits gets a few ulps of slack in the line breaker, so it stays on one line. Backports upstream #1375 and #1449. |

0002–0006 are not upstream yet, except 0004's float slot clamp and shrink-to-fit
float widths, which backport upstream #1443 and #980, and 0003's table rebuild on border, padding and size changes,
which backports #1276's `table_needs_rebuild`, and 0005's clamped `<img>` edges, which
backport open upstream #1201. Upstream main adds `cellspacing` and
`cellpadding` hints (#1366), which overlap 0003. 0007 backports upstream #1374
and the `<link>` part of #1318; its `@import` change is local. 0008 backports
upstream #1342, keeping beta.4's element transform, and open upstream #1372. 0009 to 0033 are local, and upstream main collects selection ranges as beta.4 does. 0034 backports upstream #1375 and #1449. Upstream main paints text shadows too (#984), which overlaps 0019. Drop the backported parts
when a release contains them.

Copied text, find in the reader and accessible names leave out 0011's edge
placeholders (`blitz_dom::node::without_inline_edges`). Find in the reader highlights
its matches through 0015's `set_text_highlights`. The reader's CPU renderer blurs
0019's shadows with vello_cpu's filters, and `src/renderer/cpu_scene.rs` hands it
only the blurs and drop shadows it runs, since it panics on colour matrices. The
mail sanitizer keeps the presentational attributes these patches map, such as
`background`, `clear`, `frame`, `rules`, `noshade`, `type`, `reversed` and `value`,
and elements ammonia's defaults leave out, such as `section` and `main`, as
other mail clients' sanitizers do.

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
tables and bevels, 0013's image minimums and line breaks, 0014's table
border attributes and spare width, 0015's selection line breaks and
highlights, 0016's cell percentage padding and legacy word-break, 0017's
unavailable images, 0018's rules and bevels, 0019's text shadows, 0021's
lists, 0022's `background` and `clear` attributes, 0023's soft hyphens, 0024's
images without a source, 0025's percentage columns and cell `max-width`, 0026's
blocks inside inline elements, 0027's table baselines, 0028's table widths, 0029's collapsed borders below
captions, 0030's details and summary, 0031's line box content, 0032's
`:lang()`, 0033's superscript and subscript shifts and 0034's shrink-to-fit
widths.
Application renderer tests cover float
wrapping, selection, image invalidation, caching, backgrounds, and emoji.
Refresh Flatpak's `cargo-sources.json` whenever the root lockfile changes.

On Linux, the beta.4 queue passed the Blitz library suite (88 tests), the
focused upgrade suite (nine tests), patch reproduction, all three locked
metadata checks and the Flatpak source check. The application crate is
validated by CI.
