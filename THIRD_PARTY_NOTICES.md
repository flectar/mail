# Third-party notices

Flectar Mail contains third-party open-source software. The source and binary
forms remain subject to the licenses and copyright notices of their respective
authors.

## Slint

Slint 1.17 is copyright © SixtyFPS GmbH and Slint contributors. Flectar Mail
uses Slint under the `GPL-3.0-only` option offered by the Slint crates. The
license text is in [`LICENSES/GPL-3.0-only.txt`](LICENSES/GPL-3.0-only.txt), and
the upstream source is <https://github.com/slint-ui/slint>.

## Blitz

The locally patched `blitz-dom` and `blitz-paint` crates originate from the
DioxusLabs Blitz project and are used under their `Apache-2.0` option. The
license text is in [`LICENSES/Apache-2.0.txt`](LICENSES/Apache-2.0.txt), and the
upstream source is <https://github.com/DioxusLabs/blitz>. The pinned release,
checksums, and patch order are recorded in
[`upstream.toml`](patches/blitz/upstream.toml) and
[`series`](patches/blitz/series).

## Reader integration dependencies

The reader integration uses these Rust packages under their Apache-2.0
license option. Their source distributions retain the authors' copyright
notices and license texts; the Apache-2.0 license is also included in
[`LICENSES/Apache-2.0.txt`](LICENSES/Apache-2.0.txt).

| Package | Locked version | Upstream source | Purpose |
| --- | --- | --- | --- |
| arboard | 3.6.1 | [1Password/arboard](https://github.com/1Password/arboard) | Desktop HTML and primary-selection clipboard |
| html5ever | 0.39.0 | [servo/html5ever](https://github.com/servo/html5ever) | HTML preflight and export tokenization |
| unicode-segmentation | 1.13.3 | [unicode-rs/unicode-segmentation](https://github.com/unicode-rs/unicode-segmentation) | Unicode word selection |
| percent-encoding | 2.3.2 | [servo/rust-url](https://github.com/servo/rust-url/) | Fragment and mailto decoding |
| base64 | 0.22.1 | [marshallpierce/rust-base64](https://github.com/marshallpierce/rust-base64) | Export CSP hash encoding |

## PDF preview

Desktop, Android, and iOS packages include a non-V8 PDFium runtime from
[bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries), built
from [PDFium](https://pdfium.googlesource.com/pdfium/). The selected release and
per-platform SHA-256 digests are pinned in `scripts/stage-pdfium.py`. Every
package retains the archive's `LICENSE`, complete `licenses/` directory, and
build provenance under `pdfium-licenses` beside the private desktop library,
Android assets at `licenses/PDFium`, and iOS bundle resources at `Licenses/PDFium`.
PDFium's third-party components retain their respective licenses.

The Rust interface is [pdfium-render](https://github.com/ajrcarey/pdfium-render)
0.9.4, used under its Apache-2.0 option. Linux process restrictions use
[Landlock](https://github.com/landlock-lsm/rust-landlock) and
[seccompiler](https://github.com/rust-vmm/seccompiler), under their Apache-2.0
options. Rust package versions and source checksums are recorded in `Cargo.lock`.

## Phosphor Icons

The bundled SVG icons in `ui/icons/phosphor` are sourced from
[Phosphor Icons](https://github.com/phosphor-icons/core) at revision
`2b75f3ad12b420c9504ef05df8d2564a28f8500e` and are licensed under the MIT
License. Copyright © 2023 Phosphor Icons.

## Google Sans Flex

The bundled Google Sans Flex font is distributed under the SIL Open Font
License 1.1. Its license, provenance, and checksum are retained in
[`resources/fonts/google-sans-flex`](resources/fonts/google-sans-flex).

## Noto Emoji

The bundled Noto Emoji font is distributed under the SIL Open Font License
1.1. Its license, provenance, and checksum are retained in
[`resources/fonts/noto-emoji`](resources/fonts/noto-emoji).

## Screenshot examples

The fictional screenshot fixture uses real public sender domains so the same
favicon service used by the application can demonstrate sender recognition.
Third-party names and marks belong to their respective owners. Their appearance
does not imply affiliation, sponsorship, or endorsement.

Additional Rust dependencies are identified in `Cargo.lock` and retain the
license terms supplied by their upstream packages.
