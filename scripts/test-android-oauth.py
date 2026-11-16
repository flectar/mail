#!/usr/bin/env python3
"""Validate Android OAuth configuration and the actual build-script environment."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
SCRIPTS = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("oauth", SCRIPTS / "verify-android-oauth.py")
oauth = importlib.util.module_from_spec(spec)
spec.loader.exec_module(oauth)
GOOGLE = "FLECTAR_GOOGLE_ANDROID_CLIENT_ID"
MICROSOFT = "FLECTAR_MICROSOFT_ANDROID_CLIENT_ID"
REDIRECT = "FLECTAR_MICROSOFT_ANDROID_REDIRECT_URI"
VALID = {
    GOOGLE: "123456789-fixture.apps.googleusercontent.com",
    MICROSOFT: "abcdef01-2345-4678-9abc-def012345678",
}


class AndroidOAuthTests(unittest.TestCase):
    def test_optional_providers_and_production_requirements(self):
        for values in ({}, {GOOGLE: "", MICROSOFT: " "}, {GOOGLE: VALID[GOOGLE]}, {MICROSOFT: VALID[MICROSOFT]}):
            oauth.verify(values)
            with self.assertRaises(ValueError):
                oauth.verify(values, require_all=True)
        oauth.verify(VALID, require_all=True)
        oauth.verify({GOOGLE: VALID[GOOGLE], MICROSOFT: " " + VALID[MICROSOFT].upper() + " "})

    def test_rejects_placeholders_and_malformed_ids_without_echoing_values(self):
        for key, value in (
            (MICROSOFT, "00000000-0000-0000-0000-000000000000"),
            (MICROSOFT, "------------------------------------"),
            (MICROSOFT, "a" * 36),
            (MICROSOFT, VALID[MICROSOFT] + "?unexpected=value"),
            (MICROSOFT, "invalid-app-id"),
            (GOOGLE, "test.apps.googleusercontent.com"),
            (GOOGLE, "TEST.apps.googleusercontent.com"),
            (GOOGLE, "wrong-suffix"),
        ):
            with self.subTest(key=key, value=value):
                with self.assertRaises(ValueError) as error:
                    oauth.verify({key: value})
                self.assertIn(key, str(error.exception))
                self.assertNotIn(value, str(error.exception))

    def run_build(self, script, configuration, ci_variable=None, preview=False, ndk_revision="30.0.16248370"):
        # Run the real shell entry points. Replace Cargo with a recorder that
        # stops before compilation; no SDK downloads, signing or network calls.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "scripts").mkdir()
            for name in (script, "verify-android-oauth.py", "verify-android-keystore.sh", "android-version-code.py"):
                shutil.copyfile(SCRIPTS / name, root / "scripts" / name)
            # Exercise the real CI signing guard with a fake public certificate,
            # without depending on release secrets or a JDK.
            certificate = b"OAuth test public certificate"
            android = root / "platform/android"
            android.mkdir(parents=True)
            (android / "app-signing-certificate.sha256").write_text(
                hashlib.sha256(certificate).hexdigest() + "\n"
            )
            sdk = root / "sdk"
            sdk.mkdir()
            toolchain = root / "ndk/toolchains/llvm/prebuilt/fixture/bin"
            toolchain.mkdir(parents=True)
            (root / "ndk/source.properties").write_text(
                f"Pkg.Desc = Android NDK\nPkg.Revision = {ndk_revision}\n"
            )
            binaries = root / "bin"
            binaries.mkdir()
            for path in (toolchain / "aarch64-linux-android26-clang", binaries / "gradle"):
                path.write_text("#!/bin/sh\nexit 99\n")
                path.chmod(0o755)
            keytool = binaries / "keytool"
            keytool.write_text(
                "#!/usr/bin/env python3\nimport pathlib, sys\n"
                "assert sys.argv[1] in ('-exportcert', '-certreq')\n"
                "assert sys.argv[2] == '-keystore'\n"
                "assert sys.argv[4:8] == ['-alias', 'fixture', '-storepass:env', 'FLECTAR_ANDROID_KEYSTORE_PASSWORD']\n"
                "assert pathlib.Path(sys.argv[3]).is_file()\n"
                f"sys.stdout.buffer.write({certificate!r})\n"
            )
            keytool.chmod(0o755)
            capture = root / "cargo-environment.json"
            cargo = binaries / "cargo"
            cargo.write_text(
                "#!/usr/bin/env python3\nimport os,json,pathlib\n"
                "pathlib.Path(os.environ['OAUTH_CAPTURE']).write_text(json.dumps({"
                "key:os.environ.get(key) for key in " + repr((GOOGLE, MICROSOFT, REDIRECT)) + "}))\n"
                "raise SystemExit(73)\n"
            )
            cargo.chmod(0o755)
            keystore = root / "test.keystore"
            keystore.touch()
            env = {key: value for key, value in os.environ.items() if not key.startswith("FLECTAR_")}
            env.update({
                # Select the build mode explicitly instead of inheriting the
                # test runner's CI environment.
                "CI": "",
                "GITHUB_ACTIONS": "",
                "PATH": str(binaries) + os.pathsep + env.get("PATH", ""),
                "ANDROID_HOME": str(sdk),
                "ANDROID_NDK_ROOT": str(root / "ndk"),
                "GRADLE_CMD": str(binaries / "gradle"),
                "XDG_DATA_HOME": str(root / "data"),
                "FLECTAR_ANDROID_KEYSTORE": str(keystore),
                "FLECTAR_ANDROID_KEYSTORE_PASSWORD": "fixture",
                "FLECTAR_ANDROID_KEY_ALIAS": "fixture",
                "FLECTAR_ANDROID_KEY_PASSWORD": "fixture",
                "FLECTAR_ANDROID_VERSION_NAME": "0.1.0-alpha.8" if preview else "0.1.0",
                "OAUTH_CAPTURE": str(capture),
                **configuration,
            })
            if ci_variable:
                env[ci_variable] = "true"
            command = ["bash", str(root / "scripts" / script)]
            if preview:
                command.append("--preview")
            result = subprocess.run(command,
                                    env=env, capture_output=True, text=True, timeout=15)
            recorded = json.loads(capture.read_text()) if capture.exists() else None
            return result, recorded

    def test_preview_never_injects_placeholder_defaults(self):
        for ci_variable in (None, "CI", "GITHUB_ACTIONS"):
            for values in ({}, {GOOGLE: "", MICROSOFT: "", REDIRECT: ""}, VALID):
                with self.subTest(ci_variable=ci_variable, configuration=values):
                    result, recorded = self.run_build("build-android-release.sh", values, ci_variable, preview=True)
                    self.assertEqual(result.returncode, 73, result.stdout + result.stderr)
                    self.assertEqual(recorded, {key: values.get(key) for key in (GOOGLE, MICROSOFT, REDIRECT)})

    def test_preview_rejects_invalid_ids_before_compilation(self):
        for ci_variable in (None, "CI", "GITHUB_ACTIONS"):
            for values in ({MICROSOFT: "00000000-0000-0000-0000-000000000000"}, {GOOGLE: "test.apps.googleusercontent.com"}):
                with self.subTest(ci_variable=ci_variable, configuration=values):
                    result, recorded = self.run_build("build-android-release.sh", values, ci_variable, preview=True)
                    self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                    self.assertIsNone(recorded)
                    self.assertIn("not a valid", result.stderr)

    def test_production_requires_real_ids_before_compilation(self):
        values = {**VALID, REDIRECT: "msauth://com.flectar.mail/fixture-signature"}
        result, recorded = self.run_build("build-android-release.sh", values)
        self.assertEqual(result.returncode, 73, result.stdout + result.stderr)
        self.assertEqual(recorded, values)
        for invalid in ("", "00000000-0000-0000-0000-000000000000", "a" * 36):
            result, recorded = self.run_build("build-android-release.sh", {**values, MICROSOFT: invalid})
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertIsNone(recorded)

    def test_ndk_older_than_r30_is_refused_before_compilation(self):
        # Slint's prebuilt Skia binaries need the libc++ of NDK r30 or newer.
        for script, preview in (("build-android-release.sh", True), ("build-android-test-apk.sh", False)):
            with self.subTest(script=script):
                result, recorded = self.run_build(script, VALID, preview=preview, ndk_revision="27.3.13750724")
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIsNone(recorded)
                self.assertIn("NDK r30 or newer is required", result.stderr)

    def test_stable_cannot_bypass_provider_checks_or_override_version_order(self):
        for values in (
            {"FLECTAR_ANDROID_VERSION_NAME": "0.1.0"},
            {"FLECTAR_ANDROID_VERSION_CODE": "1"},
        ):
            result, recorded = self.run_build("build-android-release.sh", values, preview=True)
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertIsNone(recorded)


if __name__ == "__main__":
    unittest.main()
