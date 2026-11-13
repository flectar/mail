#!/usr/bin/env python3
"""Generate and check the alternate app icons offered in Settings, the macOS
icon set and the Microsoft Store package logos.

Every variant is the master icon with its own colors; the default orange icon
is the master itself and is not generated. Outputs:

- resources/app-icon/variants: masked SVGs and the window, tray and 512 px
  PNGs used on the desktop;
- resources/app-icon/flectar-mail.iconset: the master icon at every size of
  the macOS .icns;
- platform/ios/Assets.xcassets/AppIcon<Name>.appiconset: opaque 1024 px
  alternate icons;
- platform/android/.../res: adaptive launcher icons for the launcher aliases;
- platform/windows/msix/Assets: the master icon's logos for the Store package.

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
WINDOWS_ASSETS = PROJECT / "platform/windows/msix/Assets"

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

# Apple's macOS app icon template (the macOS Sequoia production templates in
# Apple Design Resources) leaves a margin around the rounded square at every
# size of the .icns, with a black drop shadow at 30% opacity in it. Each
# canvas size maps to the rounded square's size and the shadow's offset and
# blur. The Dock and the app switcher show the whole canvas, so an icon
# without the margin looks larger than other apps' icons.
MACOS_GRID = {
    16: (14, None),
    32: (28, (1, 1)),
    64: (52, (1.5, 1)),
    128: (104, (2, 1.5)),
    256: (206, (3, 3)),
    512: (412, (5, 5)),
    1024: (824, (10, 10)),
}
MACOS_SHADOW_OPACITY = 77 / 255
# The template's corners are less round than the master's mask. Along the
# diagonal, the rounded square's edge is this fraction of its size away from
# its corner. Shrinking the master's corners to that keeps their curve and
# matches the template's mask to within half a pixel at 1024 px.
MACOS_CORNER_INSET = 54.06 / 824
MACOS_ICONSET = ROOT / "flectar-mail.iconset"
# The file names iconutil expects, with their canvas sizes.
ICONSET_FILES = {
    "icon_16x16.png": 16,
    "icon_16x16@2x.png": 32,
    "icon_32x32.png": 32,
    "icon_32x32@2x.png": 64,
    "icon_128x128.png": 128,
    "icon_128x128@2x.png": 256,
    "icon_256x256.png": 256,
    "icon_256x256@2x.png": 512,
    "icon_512x512.png": 512,
    "icon_512x512@2x.png": 1024,
}
SVG_START = '<svg width="1024" height="1024" viewBox="0 0 1024 1024" fill="none" xmlns="http://www.w3.org/2000/svg">'
MASK = re.compile(r'(<clipPath id="appleCanvasMask"[^>]*>\s*<path d=")([^"]+)" transform="translate\(([-\d.]+) ([-\d.]+)\)"')



def windows_logos():
    """Store package logo file names, with their pixel size and icon scale.

    These are the sizes Microsoft's app icon guidance lists. Windows uses an
    exact size when it has one and otherwise shrinks the next larger file, so
    every size the taskbar, Start and title bars use at each display scale is
    rendered from the SVG. The altform-unplated (dark theme) and
    altform-lightunplated files stop Windows from drawing a plate behind the
    icon. Start menu tiles keep the icon at half size.
    """
    logos = {}
    for scale in (100, 125, 150, 200, 400):
        for name, base, icon in (("StoreLogo", 50, 1), ("Square44x44Logo", 44, 1), ("Square150x150Logo", 150, 0.5)):
            logos[f"{name}.scale-{scale}.png"] = (int(base * scale / 100 + 0.5), icon)
    for size in (16, 20, 24, 30, 32, 36, 40, 48, 60, 64, 72, 80, 96, 256):
        for form in ("", "_altform-unplated", "_altform-lightunplated"):
            logos[f"Square44x44Logo.targetsize-{size}{form}.png"] = (size, 1)
    return logos


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
    rasters.update({MACOS_ICONSET / file: (size, 6) for file, size in ICONSET_FILES.items()})
    rasters.update({IOS_ASSETS / f"AppIcon{title(name)}.appiconset/AppIcon-1024.png": (1024, 2)
                    for name in VARIANTS})
    rasters.update({WINDOWS_ASSETS / name: (size, 6) for name, (size, _) in windows_logos().items()})
    return rasters


def macos_mask(path, offset_x, offset_y, source):
    """The master's mask with the corners of Apple's macOS icon template."""
    tokens = re.findall(r"[MCVHZ]|-?(?:\d+\.?\d*|\.\d+)", path)
    segments = []
    command = None
    x = y = 0.0
    position = 0
    while position < len(tokens):
        if tokens[position] in "MCVHZ":
            command = tokens[position]
            position += 1
            if command == "Z":
                segments.append(("Z", []))
                continue
        count = {"M": 2, "C": 6, "V": 1, "H": 1}.get(command)
        if count is None or position + count > len(tokens):
            raise ValueError(f"Unexpected mask path in {source}")
        values = [float(token) for token in tokens[position:position + count]]
        position += count
        if command == "V":
            y = values[0] + offset_y
            segments.append(("L", [(x, y)]))
        elif command == "H":
            x = values[0] + offset_x
            segments.append(("L", [(x, y)]))
        else:
            points = [(values[index] + offset_x, values[index + 1] + offset_y)
                      for index in range(0, count, 2)]
            x, y = points[-1]
            segments.append((command, points))
    # The bottom right corner's point on the diagonal.
    corner = max((points[-1] for _, points in segments if points), key=sum)
    if abs(corner[0] - corner[1]) > 0.01:
        raise ValueError(f"The mask in {source} no longer has a point on the diagonal")
    scale = MACOS_CORNER_INSET * 1024 / (1024 - corner[0])

    def place(value):
        return value * scale if value < 512 else 1024 - (1024 - value) * scale

    return " ".join(command + " ".join(f"{place(px):.3f} {place(py):.3f}" for px, py in points)
                    for command, points in segments)


def macos_svg(svg, canvas, source):
    """Draw a masked icon on Apple's macOS icon grid at `canvas` pixels."""
    body, shadow = MACOS_GRID[canvas]
    opacity = MACOS_SHADOW_OPACITY
    mask = MASK.search(svg)
    end = svg.rstrip().rfind("</svg>")
    if not svg.startswith(SVG_START) or not mask or end < 0:
        raise ValueError(f"{source} no longer has the 1024-unit <svg> element and appleCanvasMask clip path")
    path = macos_mask(mask.group(2), float(mask.group(3)), float(mask.group(4)), source)
    drawing = svg[len(SVG_START):mask.start()] + mask.group(1) + path + '"' + svg[mask.end():end]
    margin = (canvas - body) // 2
    group = f'<g transform="translate({margin} {margin}) scale({body / 1024})">{drawing}</g>'
    if shadow:
        offset, blur = shadow
        group = (f'<defs>\n  <filter id="macosShadow" x="0" y="0" width="{canvas}" height="{canvas}" '
                 'filterUnits="userSpaceOnUse">\n'
                 f'    <feDropShadow dx="0" dy="{offset:g}" stdDeviation="{blur:g}" flood-color="#000000" '
                 f'flood-opacity="{opacity:.4f}"/>\n  </filter>\n</defs>\n'
                 f'<g filter="url(#macosShadow)">{group}</g>')
    return (f'<svg width="{canvas}" height="{canvas}" viewBox="0 0 {canvas} {canvas}" fill="none" '
            f'xmlns="http://www.w3.org/2000/svg">\n{group}\n</svg>\n')


def scaled_svg(svg, scale, source):
    """Center the 1024-unit master at `scale` of its canvas."""
    side = round(1024 / scale)
    margin = (side - 1024) // 2
    return replace_once(
        svg, '<svg width="1024" height="1024" viewBox="0 0 1024 1024"',
        f'<svg width="{side}" height="{side}" viewBox="-{margin} -{margin} {side} {side}"', source)


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
        for directory in (VARIANTS_DIR, MACOS_ICONSET, WINDOWS_ASSETS):
            unexpected = {path.name for path in directory.glob("*")} - {
                path.name for path in [*texts, *rasters] if path.parent == directory}
            if unexpected:
                raise ValueError(f"Unexpected files in {directory.relative_to(PROJECT)}: "
                                 + ", ".join(sorted(unexpected)))
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
            print(f"Checked {len(VARIANTS)} icon variants, the macOS icon set and the Store logos: "
                  f"{len(texts)} files and {len(rasters)} rasters")
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
            masked = (ROOT / "flectar-mail-masked.svg").read_text(encoding="utf-8")
            MACOS_ICONSET.mkdir(exist_ok=True)
            for file, size in ICONSET_FILES.items():
                svg = Path(directory) / f"macos-{file}.svg"
                svg.write_text(macos_svg(masked, size, "flectar-mail-masked.svg"), encoding="utf-8")
                rasterize(svg, MACOS_ICONSET / file, size)
            WINDOWS_ASSETS.mkdir(parents=True, exist_ok=True)
            for name, (size, scale) in windows_logos().items():
                svg = Path(directory) / f"windows-{scale}.svg"
                svg.write_text(scaled_svg(masked, scale, "flectar-mail-masked.svg"), encoding="utf-8")
                rasterize(svg, WINDOWS_ASSETS / name, size)
        check_declarations()
        print(f"Generated {len(VARIANTS)} icon variants, the macOS icon set and the Store logos: "
              f"{len(texts)} files and {len(rasters)} rasters")
        return 0
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
