#!/usr/bin/env python3
"""Build the bitmap strike test fonts in resources/test-fonts.

Calibri and Cambria pair their outlines with monochrome bitmap strikes in
EBLC/EBDT tables. These fonts reproduce that structure from the bundled
Google Sans Flex (OFL 1.1): the default instance subset to E, F and space,
renamed, with one 16 ppem strike. F is byte-aligned (EBDT image format 1, as
in Calibri) and E bit-aligned (format 2). The CBDT variant stores the same
masks in CBLC/CBDT, as a color bitmap font could.

Needs FontTools. Run from the repository root:

    python3 scripts/make-bitmap-strike-test-fonts.py
"""

from pathlib import Path
import struct

from fontTools.subset import Options, Subsetter
from fontTools.ttLib import TTFont
from fontTools.ttLib.tables.DefaultTable import DefaultTable
from fontTools.varLib.instancer import instantiateVariableFont

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "resources/fonts/google-sans-flex/GoogleSansFlex-VariableFont_GRAD,ROND,opsz,slnt,wdth,wght.ttf"
OUT = ROOT / "resources/test-fonts"
PPEM = 16

# 7 x 11 one-bit glyphs, top row first.
F = [
    "1111111",
    "1111111",
    "1100000",
    "1100000",
    "1100000",
    "1111110",
    "1111110",
    "1100000",
    "1100000",
    "1100000",
    "1100000",
]
E = [
    "1111111",
    "1111111",
    "1100000",
    "1100000",
    "1100000",
    "1111110",
    "1111110",
    "1100000",
    "1100000",
    "1111111",
    "1111111",
]


def small_metrics(rows):
    # height, width, bearingX, bearingY, advance
    return struct.pack(">BBbbB", len(rows), len(rows[0]), 1, len(rows), len(rows[0]) + 2)


def byte_aligned(rows):
    data = b""
    for row in rows:
        bits = row.ljust((len(row) + 7) // 8 * 8, "0")
        data += bytes(int(bits[i:i + 8], 2) for i in range(0, len(bits), 8))
    return data


def bit_aligned(rows):
    bits = "".join(rows)
    bits = bits.ljust((len(bits) + 7) // 8 * 8, "0")
    return bytes(int(bits[i:i + 8], 2) for i in range(0, len(bits), 8))


def line_metrics():
    # ascender, descender, widthMax, caret slope numerator/denominator, caret
    # offset, minOriginSB, minAdvanceSB, maxBeforeBL, minAfterBL, pad1, pad2
    return struct.pack(">bbBbbbbbbbbb", 12, -4, 9, 1, 0, 0, 1, 1, 11, 0, 0, 0)


def strike_tables(major, glyphs):
    """EBLC/EBDT (major 2) or CBLC/CBDT (major 3) for (glyph id, format, data) entries."""
    glyphs = sorted(glyphs)
    data = struct.pack(">HH", major, 0)
    subtables = []
    for glyph_id, image_format, image in glyphs:
        offset = len(data)
        data += image
        # Index format 1: header, then offsets of this glyph and of its end.
        subtables.append((glyph_id, struct.pack(">HHIII", 1, image_format, offset, 0, len(image))))
    size_offset = 8
    array_offset = size_offset + 48
    array = b""
    tables = b""
    first_table = 8 * len(subtables)
    for glyph_id, table in subtables:
        array += struct.pack(">HHI", glyph_id, glyph_id, first_table + len(tables))
        tables += table
    size = struct.pack(">IIII", array_offset, len(array) + len(tables), len(subtables), 0)
    size += line_metrics() + line_metrics()
    size += struct.pack(">HHBBBb", glyphs[0][0], glyphs[-1][0], PPEM, PPEM, 1, 1)
    location = struct.pack(">HHI", major, 0, 1) + size + array + tables
    return location, data


def raw_table(tag, data):
    table = DefaultTable(tag)
    table.data = data
    return table


def base_font(family):
    font = TTFont(SOURCE)
    defaults = {axis.axisTag: None for axis in font["fvar"].axes}
    font = instantiateVariableFont(font, defaults, static=True)
    options = Options()
    options.layout_features = []
    options.name_IDs = ["*"]
    options.notdef_outline = True
    subsetter = Subsetter(options)
    subsetter.populate(unicodes=[0x20, 0x45, 0x46])
    subsetter.subset(font)
    postscript = family.replace(" ", "")
    for record in font["name"].names:
        if record.nameID in (1, 16):
            record.string = family
        elif record.nameID in (4, 18):
            record.string = family
        elif record.nameID == 6:
            record.string = postscript
        elif record.nameID == 3:
            record.string = postscript + "-test"
    return font


def build(name, family, major, location_tag, data_tag):
    font = base_font(family)
    cmap = font.getBestCmap()
    glyph_id = font.getGlyphID
    location, data = strike_tables(major, [
        (glyph_id(cmap[0x46]), 1, small_metrics(F) + byte_aligned(F)),
        (glyph_id(cmap[0x45]), 2, small_metrics(E) + bit_aligned(E)),
    ])
    font[location_tag] = raw_table(location_tag, location)
    font[data_tag] = raw_table(data_tag, data)
    path = OUT / name
    # Keep the source's timestamp, so the output is reproducible.
    font.recalcTimestamp = False
    font.save(path)
    print(path.relative_to(ROOT), path.stat().st_size, "bytes")


build("BitmapStrikes-EBDT.ttf", "Flectar EBDT Strikes Test", 2, "EBLC", "EBDT")
build("BitmapStrikes-CBDT.ttf", "Flectar CBDT Strikes Test", 3, "CBLC", "CBDT")
