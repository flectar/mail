#!/usr/bin/env python3
"""Generate and check the alternate app icons offered in Settings.

Every variant is the master icon with its own colors; the default orange icon
is the master itself and is not generated. Outputs:

- resources/app-icon/variants: masked SVGs and the window, tray and 512 px
  PNGs used on the desktop;
- platform/ios/Assets.xcassets/AppIcon<Name>.appiconset: opaque 1024 px
  alternate icons;
- platform/android/.../res: adaptive launcher icons for the launcher aliases.

Rasterizing needs the resvg command-line tool (`cargo install resvg`); --check
only reads files, and also checks that the Android manifest and the iOS
project declare every icon.
"""

import argparse
from pathlib import Path
import re
import struct
import subprocess
import sys
import tempfile
import zlib


ROOT = Path(__file__).resolve().parent
PROJECT = ROOT.parents[1]
VARIANTS_DIR = ROOT / "variants"
IOS_ASSETS = PROJECT / "platform/ios/Assets.xcassets"
IOS_PROJECT = PROJECT / "platform/ios/project.yml"
ANDROID_MAIN = PROJECT / "platform/android/gradle/app/src/main"
ANDROID_RES = ANDROID_MAIN / "res"

# Background gradients run from the bottom stop to the top stop, like the
# master's. A glyph gradient replaces the white envelope.
VARIANTS = {
    "graphite": {"background": ("#121214", "#6C6F76")},
    "light": {"background": ("#E4E4E9", "#FFFFFF"), "glyph": ("#FF3F0A", "#FD8D0E")},
    "blue": {"background": ("#0A4FE0", "#38A1FF")},
    "green": {"background": ("#08814A", "#36C978")},
    "purple": {"background": ("#5A22D4", "#A65CF6")},
}
ICONS = ["orange", *VARIANTS]

# Logical name, pixel size. The window and tray sizes match the master's
# flectar-mail-window.png and flectar-mail-tray.png; 512 px feeds the Dock and
# the Linux icon theme.
RASTERS = (("512", 512), ("window", 256), ("tray", 64))

MASTER_STOPS = ('<stop stop-color="#FF3F0A"/>', '<stop offset="1" stop-color="#FD8D0E"/>')
GLYPH_FILL = re.compile(r'(<path d="M[^"]*") fill="white"')
ANDROID_STOPS = ('android:startColor="#FF3F0A"', 'android:endColor="#FD8D0E"')
ANDROID_GLYPH = re.compile(
    r'<path\n(\s+)android:fillColor="#FFFFFFFF"\n\s+android:pathData="([^"]*)" />')


def title(name):
    return name[0].upper() + name[1:]


def replace_once(text, old, new, source):
    if text.count(old) != 1:
        raise ValueError(f"{source} no longer has exactly one {old}")
    return text.replace(old, new)


def variant_svg(master, colors, source):
    bottom, top = colors["background"]
    svg = replace_once(master, MASTER_STOPS[0], f'<stop stop-color="{bottom}"/>', source)
    svg = replace_once(svg, MASTER_STOPS[1], f'<stop offset="1" stop-color="{top}"/>', source)
    if "glyph" in colors:
        bottom, top = colors["glyph"]
        # The envelope is drawn in a 24-unit grid; run the gradient over its
        # 4..20 vertical extent so it matches the master's background.
        svg = replace_once(svg, "</defs>", (
            '  <linearGradient id="glyph" x1="12" y1="20" x2="12" y2="4" gradientUnits="userSpaceOnUse">\n'
            f'    <stop stop-color="{bottom}"/>\n'
            f'    <stop offset="1" stop-color="{top}"/>\n'
            '  </linearGradient>\n'
            '</defs>'), source)
        svg, count = GLYPH_FILL.subn(r'\1 fill="url(#glyph)"', svg)
        if count != 2:
            raise ValueError(f"Expected two white envelope paths in {source}, found {count}")
    return svg


def android_files():
    background = (ANDROID_RES / "drawable/ic_launcher_background.xml").read_text(encoding="utf-8")
    foreground = (ANDROID_RES / "drawable/ic_launcher_foreground.xml").read_text(encoding="utf-8")
    files = {}
    for name, colors in VARIANTS.items():
        bottom, top = colors["background"]
        source = "ic_launcher_background.xml"
        drawable = replace_once(background, ANDROID_STOPS[0], f'android:startColor="{bottom}"', source)
        drawable = replace_once(drawable, ANDROID_STOPS[1], f'android:endColor="{top}"', source)
        files[f"drawable/ic_launcher_background_{name}.xml"] = drawable
        glyph = "ic_launcher_foreground"
        if "glyph" in colors:
            bottom, top = colors["glyph"]
            glyph = f"ic_launcher_foreground_{name}"

            def gradient(match):
                indent = match.group(1)
                inner = indent + "    "
                return (
                    f'<path\n{indent}android:pathData="{match.group(2)}">\n'
                    f'{indent}<aapt:attr name="android:fillColor">\n'
                    f'{inner}<gradient\n'
                    f'{inner}    android:startX="12"\n'
                    f'{inner}    android:startY="20"\n'
                    f'{inner}    android:endX="12"\n'
                    f'{inner}    android:endY="4"\n'
                    f'{inner}    android:startColor="{bottom}"\n'
                    f'{inner}    android:endColor="{top}"\n'
                    f'{inner}    android:type="linear" />\n'
                    f'{indent}</aapt:attr>\n'
                    f'{indent[:-4]}</path>')

            drawable = replace_once(
                foreground,
                'xmlns:android="http://schemas.android.com/apk/res/android"\n',
                'xmlns:android="http://schemas.android.com/apk/res/android"\n'
                '    xmlns:aapt="http://schemas.android.com/aapt"\n',
                "ic_launcher_foreground.xml")
            drawable, count = ANDROID_GLYPH.subn(gradient, drawable)
            if count != 2:
                raise ValueError(f"Expected two white envelope paths in ic_launcher_foreground.xml, found {count}")
            files[f"drawable/{glyph}.xml"] = drawable
        for directory in ("mipmap-anydpi-v26", "mipmap-anydpi-v33"):
            for shape in ("ic_launcher", "ic_launcher_round"):
                icon = (ANDROID_RES / directory / f"{shape}.xml").read_text(encoding="utf-8")
                icon = replace_once(
                    icon, '"@drawable/ic_launcher_background"',
                    f'"@drawable/ic_launcher_background_{name}"', f"{directory}/{shape}.xml")
                # The monochrome layer keeps the plain envelope's shape.
                icon = replace_once(
                    icon, '<foreground android:drawable="@drawable/ic_launcher_foreground" />',
                    f'<foreground android:drawable="@drawable/{glyph}" />', f"{directory}/{shape}.xml")
                files[f"{directory}/{shape}_{name}.xml"] = icon
    return files


IOS_CONTENTS = """{
  "images" : [
    { "filename" : "AppIcon-1024.png", "idiom" : "universal", "platform" : "ios", "size" : "1024x1024" }
  ],
  "info" : {
    "author" : "xcode",
    "version" : 1
  }
}
"""


def png_header(path):
    header = path.read_bytes()[:26]
    if header[:8] != b"\x89PNG\r\n\x1a\n" or header[12:16] != b"IHDR":
        raise ValueError(f"Not a PNG file: {path}")
    width, height, depth, color = struct.unpack(">IIBB", header[16:26])
    return width, height, depth, color


def png_chunks(data):
    position = 8
    while position < len(data):
        length, kind = struct.unpack(">I4s", data[position:position + 8])
        yield kind, data[position + 8:position + 8 + length]
        position += 12 + length


def opaque_rgb(path):
    """Rewrite an 8-bit RGBA PNG without its alpha channel. iOS requires
    opaque app icons and the App Store rejects icons with an alpha channel."""
    data = path.read_bytes()
    width, height, depth, color = png_header(path)
    if (depth, color) != (8, 6):
        raise ValueError(f"Expected an 8-bit RGBA PNG: {path}")
    raw = zlib.decompress(b"".join(body for kind, body in png_chunks(data) if kind == b"IDAT"))
    stride = width * 4
    previous = bytearray(stride)
    rows = []
    offset = 0
    for _ in range(height):
        kind = raw[offset]
        line = bytearray(raw[offset + 1:offset + 1 + stride])
        offset += 1 + stride
        for x in range(stride):
            left = line[x - 4] if x >= 4 else 0
            up = previous[x]
            corner = previous[x - 4] if x >= 4 else 0
            if kind == 1:
                line[x] = (line[x] + left) & 255
            elif kind == 2:
                line[x] = (line[x] + up) & 255
            elif kind == 3:
                line[x] = (line[x] + (left + up) // 2) & 255
            elif kind == 4:
                estimate = left + up - corner
                distances = (abs(estimate - left), abs(estimate - up), abs(estimate - corner))
                line[x] = (line[x] + (left if distances[0] <= distances[1] and distances[0] <= distances[2]
                                      else up if distances[1] <= distances[2] else corner)) & 255
        if any(line[x] != 255 for x in range(3, stride, 4)):
            raise ValueError(f"{path} is not opaque")
        rows.append(bytes(line))
        previous = line
    encoded = bytearray()
    above = bytes(width * 3)
    for row in rows:
        rgb = bytes(byte for index, byte in enumerate(row) if index % 4 != 3)
        # The Up filter suits vertical gradients.
        encoded.append(2)
        encoded.extend((value - prior) & 255 for value, prior in zip(rgb, above))
        above = rgb

    def chunk(kind, body):
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))

    path.write_bytes(b"\x89PNG\r\n\x1a\n"
                     + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
                     + chunk(b"IDAT", zlib.compress(bytes(encoded), 9))
                     + chunk(b"IEND", b""))


def expected_text():
    masked = (ROOT / "flectar-mail-masked.svg").read_text(encoding="utf-8")
    files = {VARIANTS_DIR / f"{name}.svg": variant_svg(masked, colors, "flectar-mail-masked.svg")
             for name, colors in VARIANTS.items()}
    files.update({IOS_ASSETS / f"AppIcon{title(name)}.appiconset/Contents.json": IOS_CONTENTS
                  for name in VARIANTS})
    files.update({ANDROID_RES / relative: text for relative, text in android_files().items()})
    return files


def expected_rasters():
    rasters = {VARIANTS_DIR / f"{name}-{label}.png": (size, 6)
               for name in VARIANTS for label, size in RASTERS}
    rasters.update({IOS_ASSETS / f"AppIcon{title(name)}.appiconset/AppIcon-1024.png": (1024, 2)
                    for name in VARIANTS})
    return rasters


def check_declarations():
    manifest = (ANDROID_MAIN / "AndroidManifest.xml").read_text(encoding="utf-8")
    aliases = re.findall(r'<activity-alias\s+android:name="\.Icon(\w+)"', manifest)
    if aliases != [title(name) for name in ICONS]:
        raise ValueError(f"AndroidManifest.xml declares launcher icons {aliases}, expected {ICONS}")
    project = IOS_PROJECT.read_text(encoding="utf-8")
    declared = re.search(r"ASSETCATALOG_COMPILER_ALTERNATE_APPICON_NAMES: \"([^\"]*)\"", project)
    expected = " ".join(f"AppIcon{title(name)}" for name in VARIANTS)
    if not declared or declared.group(1) != expected:
        raise ValueError(f"project.yml must list the alternate icons as \"{expected}\"")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--check", action="store_true", help="Read-only freshness and size checks")
    parser.add_argument("--resvg", default="resvg", help="resvg executable used to rasterize")
    args = parser.parse_args()
    try:
        texts = expected_text()
        rasters = expected_rasters()
        unexpected = {path.name for path in VARIANTS_DIR.glob("*")} - {
            path.name for path in [*texts, *rasters] if path.parent == VARIANTS_DIR}
        if unexpected:
            raise ValueError("Unexpected icon variant files: " + ", ".join(sorted(unexpected)))
        stale = [path for path, text in texts.items()
                 if not path.is_file() or path.read_text(encoding="utf-8") != text]
        if args.check:
            check_declarations()
            if stale:
                raise ValueError("Stale icon variants: " + ", ".join(
                    str(path.relative_to(PROJECT)) for path in stale))
            for path, (size, color) in rasters.items():
                if not path.is_file():
                    raise ValueError(f"Missing icon raster: {path.relative_to(PROJECT)}")
                width, height, _, actual = png_header(path)
                if (width, height, actual) != (size, size, color):
                    raise ValueError(f"{path.relative_to(PROJECT)} is {width}x{height} "
                                     f"(color type {actual}), expected {size}x{size} (color type {color})")
            print(f"Checked {len(VARIANTS)} icon variants: {len(texts)} files and {len(rasters)} rasters")
            return 0
        for path, text in texts.items():
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(text, encoding="utf-8")

        def rasterize(svg, png, size):
            subprocess.run([args.resvg, "--width", str(size), "--height", str(size), str(svg), str(png)],
                           check=True)

        for name in VARIANTS:
            for label, size in RASTERS:
                rasterize(VARIANTS_DIR / f"{name}.svg", VARIANTS_DIR / f"{name}-{label}.png", size)
        full_bleed = (ROOT / "flectar-mail-nonmasked.svg").read_text(encoding="utf-8")
        with tempfile.TemporaryDirectory() as directory:
            for name, colors in VARIANTS.items():
                svg = Path(directory) / f"{name}.svg"
                svg.write_text(variant_svg(full_bleed, colors, "flectar-mail-nonmasked.svg"), encoding="utf-8")
                png = IOS_ASSETS / f"AppIcon{title(name)}.appiconset/AppIcon-1024.png"
                rasterize(svg, png, 1024)
                opaque_rgb(png)
        check_declarations()
        print(f"Generated {len(VARIANTS)} icon variants: {len(texts)} files and {len(rasters)} rasters")
        return 0
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
