# Color emoji regression fixture

`NotoColorEmoji-digits.ttf` is a test-only subset of Google's Noto Color Emoji,
licensed under SIL OFL 1.1 (see `OFL-NotoEmoji.txt`). It is not bundled into the
application. The font's embedded copyright/name records are retained.

Source: https://github.com/googlefonts/noto-emoji/blob/f3ae03f5e9b3b8516fa151f7168159ca1a3e7515/fonts/NotoColorEmoji.ttf

Original SHA-256: `72a635cb3d2f3524c51620cdde406b217204e8a6a06c6a096ff8ed4b5fd6e27b`

Reproduce with FontTools (`pyftsubset`):

```sh
pyftsubset NotoColorEmoji.ttf \
  --unicodes=U+0023,U+002A,U+0030-0039,U+00A9,U+00AE,U+20E3,U+FE0E,U+FE0F,U+1F600 \
  --output-file=NotoColorEmoji-digits.ttf
```

This retains the bitmap digits, hash, asterisk, copyright/registered symbols,
keycap sequences, and grinning face. It reproduces issues #16/#23 without
depending on fonts installed on the machine: when an email names an unavailable
platform font, Parley can select
this emoji font for plain digits before reaching its Latin/Common fallback.
Vello CPU's current build lacks PNG glyph support, so these bitmap digits
vanish; Vello GPU paints them with emoji metrics and excessive spacing.
The tests verify that text uses the regular fallback while real emoji still
select the emoji family.

# Bitmap strike regression fixtures

`BitmapStrikes-EBDT.ttf` and `BitmapStrikes-CBDT.ttf` are test-only fonts
derived from the bundled Google Sans Flex, licensed under SIL OFL 1.1 (see
`OFL-GoogleSansFlex.txt`). They are not bundled into the application. Each is
the default instance subset to E, F and space, renamed to `Flectar EBDT
Strikes Test` and `Flectar CBDT Strikes Test`, with one 16 ppem monochrome
bitmap strike added, like the strikes in Calibri and Cambria. F's bitmap is
byte-aligned (image format 1, as in Calibri) and E's bit-aligned (format 2).
The EBDT font keeps them in `EBLC`/`EBDT`, the CBDT font in `CBLC`/`CBDT`.

Source: `resources/fonts/google-sans-flex`, SHA-256
`c6d53424121196b81de816b8daccf200e285dd506df43766db3d7e8cdf06ee30`

Reproduce with FontTools:

```sh
python3 scripts/make-bitmap-strike-test-fonts.py
```

Vello's GPU renderer dropped such glyphs or drew them white instead of drawing
their outlines (issue #342). The tests check that both renderers draw F and E
from their outlines.
