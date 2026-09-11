#!/usr/bin/env python3
"""Offline regressions for PDFium staging and native package validation."""
import hashlib
import importlib.util
import io
import os
import pathlib
import plistlib
import shutil
import struct
import subprocess
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

    def test_macos_library_and_licenses_use_bundle_locations(self):
        data = archive({
            "lib/libpdfium.dylib": b"library",
            "LICENSE": b"license",
            "licenses/notice": b"notice",
        })
        checksum = hashlib.sha256(data).hexdigest()
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            frameworks = root / "Flectar Mail.app/Contents/Frameworks"
            licenses = root / "Flectar Mail.app/Contents/Resources/Licenses/PDFium"
            with patch.dict(
                stage.ASSETS,
                {"fixture": (checksum, "lib/libpdfium.dylib")},
            ), patch.object(stage.urllib.request, "urlopen", return_value=io.BytesIO(data)):
                stage.stage("fixture", frameworks, licenses_destination=licenses)
            self.assertEqual((frameworks / "libpdfium.dylib").read_bytes(), b"library")
            self.assertEqual((licenses / "LICENSE").read_bytes(), b"license")
            self.assertEqual((licenses / "licenses/notice").read_bytes(), b"notice")
            self.assertTrue((licenses / "build.json").is_file())
            self.assertFalse((frameworks / "pdfium-licenses").exists())

    def test_macos_packager_signs_nested_library_before_app(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            for name in (
                "scripts/package-macos.sh",
                "platform/macos/Info.plist.in",
                "LICENSE",
                "THIRD_PARTY_NOTICES.md",
                "resources/fonts/google-sans-flex/OFL.txt",
                "resources/fonts/google-sans-flex/README.md",
                "resources/fonts/noto-emoji/OFL.txt",
                "resources/fonts/noto-emoji/README.md",
            ):
                output = root / name
                output.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(SCRIPTS.parent / name, output)
            shutil.copytree(SCRIPTS.parent / "LICENSES", root / "LICENSES")
            (root / "resources/app-icon").mkdir(parents=True)
            (root / "resources/app-icon/flectar-mail-masked.png").write_bytes(b"png")
            (root / "Cargo.toml").write_text('[package]\nversion = "0.1.0-alpha.5"\n')
            (root / "target/release").mkdir(parents=True)
            shutil.copyfile("/bin/true", root / "target/release/flectar-mail")
            (root / "target/release/flectar-mail").chmod(0o755)

            (root / "scripts/stage-pdfium.py").write_text(
                "import pathlib, sys\n"
                "assert sys.argv[1] == 'mac-arm64'\n"
                "frameworks = pathlib.Path(sys.argv[2])\n"
                "licenses = pathlib.Path(sys.argv[sys.argv.index('--licenses-destination') + 1])\n"
                "frameworks.mkdir(parents=True, exist_ok=True)\n"
                "licenses.mkdir(parents=True, exist_ok=True)\n"
                "(frameworks / 'libpdfium.dylib').write_bytes(b'library')\n"
                "(licenses / 'build.json').write_text('{}')\n"
            )
            (root / "scripts/test-pdf-preview.py").write_text(
                "import pathlib, sys\n"
                "binary = pathlib.Path(sys.argv[1])\n"
                "assert binary.is_file()\n"
                "assert (binary.parent / '../Frameworks/libpdfium.dylib').is_file()\n"
            )

            tools = root / "tools"
            tools.mkdir()

            def executable(name, contents):
                path = tools / name
                path.write_text("#!/usr/bin/env bash\nset -euo pipefail\n" + contents)
                path.chmod(0o755)

            executable("sips", 'output="${@: -1}"\nmkdir -p "$(dirname "$output")"\n: > "$output"\n')
            executable("iconutil", 'output="${@: -1}"\n: > "$output"\n')
            executable(
                "codesign",
                'target="${@: -1}"\nprintf "%s\\n" "$*" >> "$CODESIGN_LOG"\n'
                'if [[ "$target" == *.dylib ]]; then : > "$target.signed"; '
                'elif [[ "$*" == *"--sign"* ]]; then test -f "$target/Contents/Frameworks/libpdfium.dylib.signed"; fi\n',
            )
            executable(
                "ditto",
                'if [[ "$1" == "-c" ]]; then : > "${@: -1}"; else mkdir -p "$2"; fi\n',
            )
            executable(
                "hdiutil",
                'if [[ "$1" == "create" ]]; then : > "${@: -1}"; else test -f "$2"; fi\n',
            )

            codesign_log = root / "codesign.log"
            result = subprocess.run(
                ["bash", str(root / "scripts/package-macos.sh")],
                cwd=root,
                env={
                    **os.environ,
                    "PATH": f"{tools}:{os.environ['PATH']}",
                    "CODESIGN_LOG": str(codesign_log),
                },
                capture_output=True,
                text=True,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            app = root / "target/macos/Flectar Mail.app/Contents"
            self.assertTrue((app / "Frameworks/libpdfium.dylib").is_file())
            self.assertTrue((app / "Resources/Licenses/PDFium/build.json").is_file())
            self.assertFalse((app / "MacOS/libpdfium.dylib").exists())
            signatures = codesign_log.read_text().splitlines()
            self.assertIn("Contents/Frameworks/libpdfium.dylib", signatures[0])
            self.assertTrue(signatures[1].endswith("Flectar Mail.app"))
            self.assertIn("--verify --deep --strict", signatures[2])

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
