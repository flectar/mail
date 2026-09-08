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
from pathlib import Path

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

    def test_debian_prerelease_package(self):
        # Exercise the actual shell packager and dpkg with a tiny existing ELF,
        # so shell expansion bugs cannot hide behind Python-only asset tests.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            paths = (
                "scripts/build-deb.sh", "LICENSE", "THIRD_PARTY_NOTICES.md",
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
