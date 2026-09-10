#!/usr/bin/env python3
"""Exercise release validation and publishing without contacting GitHub."""

import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path
from unittest.mock import patch

SCRIPTS = Path(__file__).resolve().parent
sys.dont_write_bytecode = True


def load_script(name):
    spec = importlib.util.spec_from_file_location(name, SCRIPTS / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


metadata = load_script("release-metadata").metadata
release_assets = load_script("prepare-release")
prepare = release_assets.prepare
android_signing = load_script("verify-android-signing")
verify_android_signing = android_signing.verify
linux_metadata = load_script("stage-linux-metadata")
stage_linux_metadata = linux_metadata.stage


class ReleaseTests(unittest.TestCase):
    def test_android_ci_output_and_oauth_fingerprints(self):
        signing = (SCRIPTS / "fixtures/apksigner-v2.txt").read_text()
        expected = "6f269ab047ca5d5eae7317758a265251e3d5645c5307633a5b92d192b8646280"
        for output in (signing, signing.replace("V2 Signer:", "Signer #1")):
            verify_android_signing(output, "test", expected)
            with self.assertRaises(ValueError):
                verify_android_signing(output, "test", "00" * 32)
            with self.assertRaises(ValueError):
                verify_android_signing(output, "production")
            self.assertEqual(android_signing.certificate_digest(output, "SHA-1"), "ce48c46e4aa046ef007c1f965f8797691ba40e9f")
        with self.assertRaisesRegex(ValueError, "unsupported output format"):
            verify_android_signing(signing.replace("V2 Signer:", "Unknown Signer:"), "test", expected)

        # The subsequent OAuth identity-export step must support the same SDK
        # format as APK verification, or a valid package would still fail CI.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            build_tools = root / "build-tools/36.0.0"
            build_tools.mkdir(parents=True)
            signer = build_tools / "apksigner"
            signer.write_text(f"#!/usr/bin/env python3\nprint({signing!r})\n")
            signer.chmod(0o755)
            apk = root / "app.apk"
            apk.write_bytes(b"fixture")
            output = subprocess.check_output(
                ["bash", str(SCRIPTS / "android-signing-identities.sh"), str(apk)],
                env={**os.environ, "ANDROID_HOME": str(root)}, text=True,
            )
            self.assertIn("Google Android OAuth SHA-1: CE:48:C4:6E:4A:A0:46:EF:00:7C:1F:96:5F:87:97:69:1B:A4:0E:9F", output)
            self.assertIn("Microsoft redirect URI: msauth://com.flectar.mail/", output)

    def test_android_signing_uses_certificate_not_display_name(self):
        digest = "ab" * 32
        for subject in ("CN=Flectar Mail Test, O=Android, C=US", "C=US,O=Android,CN=Flectar Mail Test"):
            signing = (
                "Verified using v2 scheme (APK Signature Scheme v2): true\n"
                "Number of signers: 1\n"
                f"Signer #1 certificate DN: {subject}\n"
                f"Signer #1 certificate SHA-256 digest: {digest}\n"
            )
            with self.subTest(subject=subject):
                verify_android_signing(signing, "test", digest.upper())
                for expected in (None, "", "invalid", "cd" * 32):
                    with self.assertRaises(ValueError):
                        verify_android_signing(signing, "test", expected)
                with self.assertRaises(ValueError):
                    verify_android_signing(signing, "production")
                with self.assertRaises(ValueError):
                    verify_android_signing(signing.replace("signers: 1", "signers: 2"), "test", digest)
                with self.assertRaises(ValueError):
                    verify_android_signing(signing.replace(": true", ": false"), "test", digest)
        production = signing.replace("CN=Flectar Mail Test", "CN=Flectar")
        verify_android_signing(production, "production")
        with self.assertRaises(ValueError):
            verify_android_signing(production.replace("CN=Flectar", "CN=Android Debug"), "production")

    def test_linux_metadata_versions(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "resources").mkdir()
            for name in ("com.flectar.mail.desktop", "com.flectar.mail.metainfo.xml"):
                shutil.copyfile(SCRIPTS.parent / "resources" / name, root / "resources" / name)
            for version in ("0.2.0-alpha.1", "0.2.0-beta.2", "0.2.0-rc.3", "0.2.0"):
                with self.subTest(version=version):
                    (root / "Cargo.toml").write_text(f'[package]\nversion = "{version}"\n')
                    destination = root / version
                    with patch.dict(os.environ, {"SOURCE_DATE_EPOCH": "1767225600"}):
                        stage_linux_metadata(root, destination)
                    desktop = (destination / "usr/share/applications/com.flectar.mail.desktop").read_text()
                    self.assertIn(f"X-AppImage-Version={version}\n", desktop)
                    release = ET.parse(destination / "usr/share/metainfo/com.flectar.mail.metainfo.xml").find("./releases/release")
                    self.assertEqual(release.get("version"), version)
                    self.assertEqual(release.get("date"), "2026-01-01")
                    self.assertEqual(release.get("type"), "development" if "-" in version else "stable")
                    # Repackaging the same source must produce identical metadata
                    # without consulting the current date.
                    repeated = root / f"{version}-repeated"
                    with patch.dict(os.environ, {"SOURCE_DATE_EPOCH": "1767225600"}):
                        with patch.object(linux_metadata, "datetime", wraps=linux_metadata.datetime) as clock:
                            clock.now.side_effect = AssertionError("Metadata must use the source date")
                            stage_linux_metadata(root, repeated)
                    for path in destination.rglob("*"):
                        if path.is_file():
                            self.assertEqual(path.read_bytes(), (repeated / path.relative_to(destination)).read_bytes())
            for name in ("com.flectar.mail.desktop", "com.flectar.mail.metainfo.xml"):
                self.assertEqual((root / "resources" / name).read_bytes(), (SCRIPTS.parent / "resources" / name).read_bytes())

    def test_linux_package_metadata(self):
        # Exercise the actual shell packager and dpkg with a tiny existing ELF,
        # so shell expansion bugs cannot hide behind Python-only asset tests.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            paths = (
                "scripts/build-deb.sh", "scripts/build-appimage.sh",
                "scripts/stage-linux-metadata.py", "resources/AppRun",
                "LICENSE", "THIRD_PARTY_NOTICES.md",
                "resources/com.flectar.mail.desktop", "resources/com.flectar.mail.metainfo.xml",
                "resources/app-icon/flectar-mail-masked-512.png",
                "resources/app-icon/flectar-mail-masked.svg",
                "resources/fonts/google-sans-flex/OFL.txt",
                "resources/fonts/google-sans-flex/README.md",
                "resources/debian/source-control.in", "resources/debian/control.in",
            )
            for name in paths:
                destination = root / name
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(SCRIPTS.parent / name, destination)
            shutil.copytree(SCRIPTS.parent / "LICENSES", root / "LICENSES")
            (root / "Cargo.toml").write_text('[package]\nversion = "0.1.0-alpha.1"\n')
            (root / "target/release").mkdir(parents=True)
            shutil.copyfile("/bin/true", root / "target/release/flectar-mail")
            # This test checks package metadata using fixture ELFs. Stub only
            # the download/renderer helpers in the disposable checkout; actual
            # native PDF rendering is exercised by the dedicated smoke tests.
            (root / "scripts/stage-pdfium.py").write_text(
                "import pathlib, shutil, sys\n"
                "destination = pathlib.Path(sys.argv[2])\n"
                "destination.mkdir(parents=True, exist_ok=True)\n"
                "shutil.copyfile('/bin/true', destination / 'libpdfium.so')\n"
            )
            (root / "scripts/test-pdf-preview.py").write_text(
                "import pathlib, sys\n"
                "assert pathlib.Path(sys.argv[1]).is_file()\n"
            )
            (root / "bin").mkdir()
            cargo = root / "bin/cargo"
            cargo.write_text("#!/bin/sh\nexit 0\n")
            cargo.chmod(0o755)
            result = subprocess.run(
                ["bash", str(root / "scripts/build-deb.sh")], cwd=root,
                env={**os.environ, "PATH": f"{root / 'bin'}:{os.environ['PATH']}"},
                capture_output=True, text=True,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            package = root / "target/deb/flectar-mail_0.1.0~alpha.1_amd64.deb"
            self.assertTrue(package.is_file())
            version = subprocess.check_output(["dpkg-deb", "--field", str(package), "Version"], text=True)
            self.assertEqual(version.strip(), "0.1.0~alpha.1")
            extracted = root / "extracted"
            subprocess.run(["dpkg-deb", "--extract", str(package), str(extracted)], check=True)
            desktop_path = "usr/share/applications/com.flectar.mail.desktop"
            metainfo_path = "usr/share/metainfo/com.flectar.mail.metainfo.xml"
            self.assertIn("X-AppImage-Version=0.1.0-alpha.1\n", (extracted / desktop_path).read_text())
            release = ET.parse(extracted / metainfo_path).find("./releases/release")
            self.assertEqual(release.get("version"), "0.1.0-alpha.1")
            self.assertEqual(release.get("type"), "development")

            # Exercise the AppImage shell packager up to its tool boundary,
            # without downloading deployment tools or compiling the application.
            deploy = root / "bin/linuxdeploy"
            deploy.write_text('''#!/usr/bin/env python3
import pathlib, sys
appdir = pathlib.Path(sys.argv[sys.argv.index("--appdir") + 1])
(appdir / "com.flectar.mail.desktop").symlink_to("usr/share/applications/com.flectar.mail.desktop")
''')
            pack = root / "bin/appimagetool"
            pack.write_text('''#!/usr/bin/env python3
import json, os, pathlib, sys
appdir = pathlib.Path(sys.argv[1])
pathlib.Path(sys.argv[2]).write_text(json.dumps({
    "version": os.environ.get("VERSION"),
    "desktop": (appdir / "com.flectar.mail.desktop").read_text(),
    "metainfo": (appdir / "usr/share/metainfo/com.flectar.mail.metainfo.xml").read_text(),
}))
''')
            deploy.chmod(0o755)
            pack.chmod(0o755)
            result = subprocess.run(
                ["bash", str(root / "scripts/build-appimage.sh")], cwd=root,
                env={**os.environ, "PATH": f"{root / 'bin'}:{os.environ['PATH']}",
                     "LINUXDEPLOY": str(deploy), "APPIMAGETOOL": str(pack), "VERSION": "stale"},
                capture_output=True, text=True,
            )
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            packaged = json.loads((root / "target/appimage/flectar-mail.AppImage").read_text())
            self.assertEqual(packaged["version"], "0.1.0-alpha.1")
            self.assertIn("X-AppImage-Version=0.1.0-alpha.1\n", packaged["desktop"])
            release = ET.fromstring(packaged["metainfo"]).find("./releases/release")
            self.assertEqual(release.get("version"), "0.1.0-alpha.1")
            self.assertEqual(release.get("type"), "development")

    def test_versions_and_tags(self):
        for version in ("0.1.0", "1.2.3-alpha.1", "1.2.3-beta.2", "1.2.3-rc.3"):
            with self.subTest(version=version):
                result = metadata(version, f"v{version}")
                self.assertEqual(result["prerelease"], str("-" in version).lower())
                self.assertEqual(result["native_version"], version.split("-")[0])
        for version, tag in (
            ("0.1.0", "v0.2.0"), ("0.1.0-beta.1", "v0.1.0"),
            ("0.1.0-beta.01", None), ("0.1.0-beta.0", None),
            ("01.2.3", None), ("1.2.3-nightly", None), ("1.2.3+build", None),
        ):
            with self.subTest(version=version, tag=tag), self.assertRaises(ValueError):
                metadata(version, tag)

    def test_staging_and_checksums(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "assets"
            source.mkdir()
            names = (
                "linux/appimage/flectar-mail.AppImage",
                "linux/deb/flectar-mail_0.1.0~beta.1_amd64.deb",
                "windows/flectar-mail-windows-x64.zip",
                "windows/flectar-mail-windows-x64-setup.exe",
                "macos/flectar-mail-macos-arm64.zip",
                "macos/flectar-mail-macos-arm64.dmg",
                "android/flectar-mail.apk",
            )
            for name in names:
                path = source / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(name.encode())
            (source / "benchmark.json").write_text("{}")
            dist = root / "dist"
            prepare(source, dist, "0.1.0-beta.1")
            self.assertEqual(len(list(dist.iterdir())), 8)
            self.assertTrue((dist / "flectar-mail-0.1.0-beta.1-android-arm64-test.apk").is_file())
            # GitHub must not rewrite a download name after we checksum it.
            deb = dist / "flectar-mail_0.1.0.beta.1_amd64.deb"
            self.assertEqual(deb.read_bytes(), (source / names[1]).read_bytes())
            for path in dist.iterdir():
                self.assertRegex(path.name, r"^[A-Za-z0-9][A-Za-z0-9._-]*$")
            result = subprocess.run(
                ["sha256sum", "--check", "SHA256SUMS"], cwd=dist,
                capture_output=True, text=True,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            missing = source / names[-1]
            missing.unlink()
            with self.assertRaises(ValueError):
                prepare(source, root / "missing", "0.1.0-beta.1")
            self.assertFalse((root / "missing").exists())
            missing.write_bytes(b"")
            with self.assertRaises(ValueError):
                prepare(source, root / "empty", "0.1.0-beta.1")
            missing.write_bytes(b"dmg")
            (source / missing.name).write_bytes(b"duplicate")
            with self.assertRaises(ValueError):
                prepare(source, root / "duplicate", "0.1.0-beta.1")

            # Stable releases keep the six desktop downloads, even if the
            # input folder happens to contain test APKs from another step.
            (source / "linux/deb/flectar-mail_0.1.0~beta.1_amd64.deb").rename(
                source / "linux/deb/flectar-mail_0.1.0_amd64.deb"
            )
            stable_dist = root / "stable"
            prepare(source, stable_dist, "0.1.0")
            self.assertEqual(len(list(stable_dist.iterdir())), 7)
            self.assertTrue((stable_dist / "flectar-mail_0.1.0_amd64.deb").is_file())
            self.assertFalse(list(stable_dist.glob("*.apk")))

    def test_notes_distinguish_android_preview(self):
        self.assertIn("test-signed", release_assets.release_notes("0.1.0-beta.1"))
        self.assertIn("prereleases only", release_assets.release_notes("0.1.0"))

    def test_metadata_rejects_stale_android_lockfile(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "platform/android").mkdir(parents=True)
            (root / "Cargo.toml").write_text('[package]\nversion = "0.2.0-beta.1"\n')
            lock = '[[package]]\nname = "flectar-mail"\nversion = "0.2.0-beta.1"\n'
            (root / "Cargo.lock").write_text(lock)
            android_lock = root / "platform/android/Cargo.lock"
            android_lock.write_text(lock.replace("0.2.0-beta.1", "0.1.0"))
            command = [sys.executable, str(SCRIPTS / "release-metadata.py"), "--tag", "v0.2.0-beta.1"]
            result = subprocess.run(command, cwd=root, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("platform/android/Cargo.lock", result.stderr)
            android_lock.write_text(lock)
            result = subprocess.run(command, cwd=root, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("prerelease=true", result.stdout)

    def publish(self, *, existing="missing", prerelease="true", fail_upload=False):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "dist").mkdir()
            (root / "dist" / "package.zip").write_bytes(b"package")
            (root / "release-notes.md").write_text("Notes")
            gh = root / "gh"
            gh.write_text('''#!/usr/bin/env python3
import json, os, sys
with open("calls.jsonl", "a") as stream:
    stream.write(json.dumps(sys.argv[1:]) + "\\n")
command = sys.argv[2]
if command == "view":
    existing = os.environ["EXISTING"]
    if existing == "missing":
        sys.exit(1)
    print(existing)
if command == "upload" and os.environ["FAIL_UPLOAD"] == "true":
    sys.exit(1)
''')
            gh.chmod(0o755)
            result = subprocess.run(
                ["bash", str(SCRIPTS / "publish-release.sh")], cwd=root,
                env={**os.environ, "PATH": f"{root}:{os.environ['PATH']}",
                     "GITHUB_REF_NAME": "v0.1.0-beta.1" if prerelease == "true" else "v0.1.0",
                     "RELEASE_PRERELEASE": prerelease, "EXISTING": existing,
                     "FAIL_UPLOAD": str(fail_upload).lower()},
                capture_output=True, text=True,
            )
            calls = [json.loads(line) for line in (root / "calls.jsonl").read_text().splitlines()]
            return result, calls

    def test_preview_and_stable_publication(self):
        for prerelease in ("true", "false"):
            with self.subTest(prerelease=prerelease):
                result, calls = self.publish(prerelease=prerelease)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual([c[1] for c in calls], ["view", "create", "upload", "edit"])
                self.assertIn("--draft", calls[1])
                self.assertIn(f"--prerelease={prerelease}", calls[-1])
                self.assertIn("--latest=" + str(prerelease == "false").lower(), calls[-1])

    def test_failed_upload_does_not_publish(self):
        result, calls = self.publish(fail_upload=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("edit", [c[1] for c in calls])

    def test_resume_draft_and_reject_published_replacement(self):
        result, calls = self.publish(existing="true")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([c[1] for c in calls], ["view", "upload", "edit"])
        result, calls = self.publish(existing="false")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual([c[1] for c in calls], ["view"])


if __name__ == "__main__":
    unittest.main()
