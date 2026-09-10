#!/usr/bin/env python3
"""Offline regressions for PDFium staging and native package validation."""
import hashlib
import importlib.util
import io
import pathlib
import plistlib
import struct
import sys
import tarfile
import tempfile
import unittest
import zipfile
from unittest.mock import patch

sys.dont_write_bytecode = True
SCRIPTS = pathlib.Path(__file__).resolve().parent


def module(name):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / (name + ".py"))
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


stage = module("stage-pdfium")
native = module("verify-android-native")
framework = module("write-pdfium-framework-plist")


def archive(files):
    result = io.BytesIO()
    with tarfile.open(fileobj=result, mode="w:gz") as output:
        for name, data in files.items():
            entry = tarfile.TarInfo(name)
            entry.size = len(data)
            output.addfile(entry, io.BytesIO(data))
    return result.getvalue()


def elf(alignment=16384):
    data = bytearray(120)
    data[:6] = b"\x7fELF\x02\x01"
    struct.pack_into("<Q", data, 32, 64)
    struct.pack_into("<HH", data, 54, 56, 1)
    struct.pack_into("<I", data, 64, 1)
    struct.pack_into("<Q", data, 112, alignment)
    return bytes(data)


class PackagingTests(unittest.TestCase):
    def test_cached_archive_is_verified_before_writing(self):
        data = archive({"lib/libpdfium.so": b"library", "LICENSE": b"license", "licenses/notice": b"notice"})
        checksum = hashlib.sha256(data).hexdigest()
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            cache = root / "cache"
            cache.mkdir()
            cached = cache / (checksum + ".tgz")
            cached.write_bytes(data)
            with patch.dict(stage.ASSETS, {"fixture": (checksum, "lib/libpdfium.so")}), patch.object(stage.urllib.request, "urlopen", side_effect=AssertionError("cached staging must be offline")):
                stage.stage("fixture", root / "output", cache)
                self.assertEqual((root / "output/libpdfium.so").read_bytes(), b"library")
                self.assertTrue((root / "output/pdfium-licenses/build.json").is_file())
                cached.write_bytes(b"corrupted archive")
                with self.assertRaisesRegex(ValueError, "checksum mismatch"):
                    stage.stage("fixture", root / "rejected", cache)
                self.assertFalse((root / "rejected").exists())

    def test_archive_traversal_is_rejected(self):
        data = archive({"licenses/../../outside": b"unexpected"})
        checksum = hashlib.sha256(data).hexdigest()
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            with patch.dict(stage.ASSETS, {"fixture": (checksum, "lib/libpdfium.so")}), patch.object(stage.urllib.request, "urlopen", return_value=io.BytesIO(data)):
                with self.assertRaisesRegex(ValueError, "Invalid PDFium archive member"):
                    stage.stage("fixture", root / "output")
            self.assertFalse((root / "outside").exists())

    def test_apk_requires_pdfium_notices_and_aligned_native_libraries(self):
        with tempfile.TemporaryDirectory() as directory:
            apk = pathlib.Path(directory) / "fixture.apk"
            files = {
                "lib/arm64-v8a/libflectar_mail_android.so": elf(),
                "lib/arm64-v8a/libpdfium.so": elf(),
                "assets/licenses/PDFium/arm64-v8a/pdfium-licenses/build.json": b"{}",
                "assets/licenses/PDFium/arm64-v8a/pdfium-licenses/LICENSE": b"license",
            }
            def write(values):
                with zipfile.ZipFile(apk, "w") as output:
                    for name, data in values.items():
                        output.writestr(name, data)
            write(files)
            native.verify_apk(apk)
            write({k: v for k, v in files.items() if not k.endswith("libpdfium.so")})
            with self.assertRaisesRegex(ValueError, "missing bundled PDFium"):
                native.verify_apk(apk)
            write({**files, "lib/arm64-v8a/libpdfium.so": elf(4096)})
            with self.assertRaisesRegex(ValueError, "not aligned"):
                native.verify_apk(apk)

    def test_ios_framework_metadata_distinguishes_device_and_simulator(self):
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "Info.plist"
            for simulator in (False, True):
                framework.write(path, simulator)
                info = plistlib.loads(path.read_bytes())
                self.assertEqual(info["CFBundleExecutable"], "PDFium")
                self.assertEqual(info["CFBundlePackageType"], "FMWK")
                self.assertEqual(info["MinimumOSVersion"], "17.0")
                self.assertEqual(info["CFBundleSupportedPlatforms"], ["iPhoneSimulator" if simulator else "iPhoneOS"])


if __name__ == "__main__":
    unittest.main()
