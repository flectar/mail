#!/usr/bin/env python3
"""Exercise the store listing check against listings Play Console would refuse."""

import importlib.util
from pathlib import Path
import struct
import sys
import tempfile
import unittest
import zlib

sys.dont_write_bytecode = True
SCRIPTS = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("listing", SCRIPTS / "check-store-listing.py")
listing = importlib.util.module_from_spec(spec)
spec.loader.exec_module(listing)


def png(path, width, height, color):
    # The check reads only the header, so the image data can stay empty.
    def chunk(kind, body):
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))

    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(b"\x89PNG\r\n\x1a\n"
                     + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, color, 0, 0, 0))
                     + chunk(b"IDAT", zlib.compress(b""))
                     + chunk(b"IEND", b""))


class StoreListingTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.locale = self.root / listing.METADATA / "en-US"
        self.locale.mkdir(parents=True)
        (self.locale / "title.txt").write_text("Flectar Mail\n")
        (self.locale / "short_description.txt").write_text("Email, calendars and contacts\n")
        (self.locale / "full_description.txt").write_text("A fast, native home for your email.\n")
        png(self.locale / "images/icon.png", 512, 512, 6)
        png(self.locale / "images/featureGraphic.png", 1024, 500, 2)
        for number in (1, 2, 3):
            png(self.locale / f"images/phoneScreenshots/{number}.png", 1080, 1920, 2)
        png(self.locale / "images/phoneScreenshots/4.png", 1920, 1080, 2)

    def problems(self):
        return "\n".join(listing.check(self.root))

    def test_complete_listing_passes(self):
        (self.locale / "changelogs").mkdir()
        (self.locale / "changelogs/1001001.txt").write_text("First beta.\n")
        translated = self.root / listing.METADATA / "es-ES"
        translated.mkdir()
        (translated / "short_description.txt").write_text("Correo, calendarios y contactos\n")
        self.assertEqual(self.problems(), "")

    def test_text_limits(self):
        for name, limit in listing.TEXTS.items():
            with self.subTest(name=name):
                original = (self.locale / name).read_text()
                (self.locale / name).write_text("x" * limit + "\n")
                self.assertEqual(self.problems(), "")
                (self.locale / name).write_text("x" * (limit + 1) + "\n")
                self.assertIn(f"{limit + 1} characters, more than {limit}", self.problems())
                (self.locale / name).write_text("")
                self.assertIn("is empty", self.problems())
                (self.locale / name).write_text(original)
        (self.locale / "changelogs").mkdir()
        (self.locale / "changelogs/1001001.txt").write_text("x" * 501)
        self.assertIn("more than 500", self.problems())
        (self.locale / "changelogs/1001001.txt").unlink()
        (self.locale / "changelogs/beta.txt").write_text("Notes")
        self.assertIn("named after a version code", self.problems())

    def test_images_play_would_refuse(self):
        for path, width, height, color, expected in (
            ("images/icon.png", 512, 512, 2, "color type 6"),
            ("images/icon.png", 1024, 1024, 6, "expected 512x512"),
            ("images/featureGraphic.png", 1024, 500, 6, "color type 2"),
            ("images/phoneScreenshots/1.png", 1080, 1920, 6, "without alpha"),
            ("images/phoneScreenshots/1.png", 720, 1280, 2, "must be 9:16 or 16:9"),
            ("images/phoneScreenshots/1.png", 1080, 2400, 2, "must be 9:16 or 16:9"),
            ("images/phoneScreenshots/1.png", 2400, 1080, 2, "must be 9:16 or 16:9"),
            ("images/phoneScreenshots/1.png", 2250, 4000, 2, "must be 9:16 or 16:9"),
        ):
            with self.subTest(path=path, size=(width, height), color=color):
                original = (self.locale / path).read_bytes()
                png(self.locale / path, width, height, color)
                self.assertIn(expected, self.problems())
                (self.locale / path).write_bytes(original)
        self.assertEqual(self.problems(), "")

    def test_screenshot_count_and_unknown_files(self):
        (self.locale / "images/phoneScreenshots/2.png").unlink()
        self.assertIn("has 3 screenshots", self.problems())
        for number in range(2, 10):
            png(self.locale / f"images/phoneScreenshots/{number}.png", 1080, 1920, 2)
        self.assertIn("has 9 screenshots", self.problems())
        for number in (9, 8):
            (self.locale / f"images/phoneScreenshots/{number}.png").unlink()
        self.assertEqual(self.problems(), "")
        png(self.locale / "images/featuregraphic.png", 1024, 500, 2)
        (self.locale / "description.txt").write_text("Typo")
        problems = self.problems()
        self.assertIn("featuregraphic.png is not a Play listing image", problems)
        self.assertIn("description.txt is not part of a store listing", problems)

    def test_default_language_is_required(self):
        (self.locale / "title.txt").unlink()
        self.assertIn("title.txt is missing", self.problems())
        self.locale.rename(self.locale.with_name("en_US"))
        self.assertIn("has no en-US listing", self.problems())


if __name__ == "__main__":
    unittest.main()
