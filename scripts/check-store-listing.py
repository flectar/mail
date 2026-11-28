#!/usr/bin/env python3
"""Check the store listing in fastlane/metadata/android against Google Play's
rules, so a listing that Play Console would refuse, or that would keep the app
out of Play's recommendations, fails here first.

Google Play takes a title of up to 30 characters, a short description of up to
80, a full description of up to 4,000 and release notes of up to 500 for each
language. It needs a 512 px icon as a 32-bit PNG and a 1024 x 500 feature
graphic without alpha. It takes two to eight phone screenshots, but shows an
app in its recommendations only with at least four, without alpha, at 9:16 or
16:9 and at least 1080 px on the short side, so those are what this asks for.
The same files are the listing F-Droid and IzzyOnDroid read.
"""

import argparse
from pathlib import Path
import re
import struct
import sys

PROJECT = Path(__file__).resolve().parents[1]
METADATA = Path("fastlane/metadata/android")
DEFAULT_LOCALE = "en-US"
TEXTS = {"title.txt": 30, "short_description.txt": 80, "full_description.txt": 4000}
RELEASE_NOTES = 500
ICON = ("images/icon.png", 512, 512, 6, 1024 * 1024)
FEATURE_GRAPHIC = ("images/featureGraphic.png", 1024, 500, 2, 15 * 1024 * 1024)
SCREENSHOTS = "images/phoneScreenshots"
SCREENSHOT_COUNT = range(4, 9)
SCREENSHOT_BYTES = 8 * 1024 * 1024


def png_header(path):
    data = path.read_bytes()[:33]
    if len(data) < 33 or data[:8] != b"\x89PNG\r\n\x1a\n" or data[12:16] != b"IHDR":
        raise ValueError(f"{path} is not a PNG")
    width, height, depth, color = struct.unpack(">IIBB", data[16:26])
    return width, height, depth, color


def text(path, limit, problems):
    try:
        value = path.read_text(encoding="utf-8")
    except UnicodeDecodeError:
        problems.append(f"{path} is not UTF-8")
        return
    value = value.removesuffix("\n")
    if not value.strip():
        problems.append(f"{path} is empty")
    elif value != value.strip():
        problems.append(f"{path} starts or ends with blank space")
    if len(value) > limit:
        problems.append(f"{path} has {len(value)} characters, more than {limit}")


def image(path, width, height, color, size, problems):
    actual = png_header(path)
    if actual != (width, height, 8, color):
        problems.append(f"{path} is {actual[0]}x{actual[1]} at {actual[2]} bits, color type {actual[3]}; "
                        f"expected {width}x{height} at 8 bits, color type {color}")
    if path.stat().st_size > size:
        problems.append(f"{path} is larger than {size // 1024} KiB")


def screenshot(path, problems):
    width, height, depth, color = png_header(path)
    short, long = sorted((width, height))
    if (depth, color) != (8, 2):
        problems.append(f"{path} must be an 8-bit RGB PNG without alpha")
    if short * 16 != long * 9 or not 1080 <= short <= 2160:
        problems.append(f"{path} is {width}x{height}; it must be 9:16 or 16:9, from 1080x1920 to 2160x3840")
    if path.stat().st_size > SCREENSHOT_BYTES:
        problems.append(f"{path} is larger than 8 MiB")


def check(root):
    metadata = root / METADATA
    problems = []
    locales = sorted(path for path in metadata.iterdir() if path.is_dir()) if metadata.is_dir() else []
    if not any(path.name == DEFAULT_LOCALE for path in locales):
        return [f"{metadata} has no {DEFAULT_LOCALE} listing"]
    for locale in locales:
        if not re.fullmatch(r"[a-z]{2,3}(-[A-Z]{2}|-[A-Za-z]{4})?", locale.name):
            problems.append(f"{locale} is not a Play language code")
        expected = set(TEXTS) | {"changelogs", "images"}
        for path in sorted(locale.iterdir()):
            if path.name not in expected:
                problems.append(f"{path} is not part of a store listing")
        for name, limit in TEXTS.items():
            path = locale / name
            if path.is_file():
                text(path, limit, problems)
            elif locale.name == DEFAULT_LOCALE:
                problems.append(f"{path} is missing")
        for path in sorted((locale / "changelogs").glob("*")):
            if not re.fullmatch(r"[1-9][0-9]*\.txt", path.name):
                problems.append(f"{path} must be named after a version code")
            else:
                text(path, RELEASE_NOTES, problems)
        images = locale / "images"
        if not images.is_dir():
            if locale.name == DEFAULT_LOCALE:
                problems.append(f"{images} is missing")
            continue
        known = {ICON[0], FEATURE_GRAPHIC[0], SCREENSHOTS}
        for path in sorted(images.iterdir()):
            if str(path.relative_to(locale)) not in known:
                problems.append(f"{path} is not a Play listing image")
        for name, width, height, color, size in (ICON, FEATURE_GRAPHIC):
            path = locale / name
            if path.is_file():
                image(path, width, height, color, size, problems)
            elif locale.name == DEFAULT_LOCALE:
                problems.append(f"{path} is missing")
        shots = sorted((locale / SCREENSHOTS).glob("*"))
        if shots or locale.name == DEFAULT_LOCALE:
            if len(shots) not in SCREENSHOT_COUNT:
                problems.append(f"{locale / SCREENSHOTS} has {len(shots)} screenshots; "
                                "Play takes up to 8 and recommends apps with at least 4")
            for path in shots:
                if path.suffix != ".png":
                    problems.append(f"{path} must be a PNG")
                else:
                    screenshot(path, problems)
    return problems


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--root", type=Path, default=PROJECT, help=argparse.SUPPRESS)
    args = parser.parse_args()
    try:
        problems = check(args.root)
    except (OSError, ValueError) as error:
        problems = [str(error)]
    if problems:
        print("\n".join(problems), file=sys.stderr)
        return 1
    print("The store listing fits Google Play's limits")
    return 0


if __name__ == "__main__":
    sys.exit(main())
