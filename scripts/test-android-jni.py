#!/usr/bin/env python3
"""Offline APK/JNI export regressions using real, stripped ELF fixtures.

Requires a host C compiler and readelf; no Android SDK or device is needed.
The fixture functions test symbol visibility only, not the callbacks' ABI.
"""
import importlib.util
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest
import zipfile

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location(
    "native", Path(__file__).with_name("verify-android-native.py")
)
native = importlib.util.module_from_spec(spec)
spec.loader.exec_module(native)


class JniExportsTests(unittest.TestCase):
    def build_library(self, directory, replacement=None, stripped=True):
        source = Path(directory) / "fixture.c"
        library = Path(directory) / "fixture.so"
        source.write_text("\n".join(
            replacement[1] if replacement and name == replacement[0]
            else f"void {name}(void) {{}}"
            for name in sorted(native.REQUIRED_EXPORTS)
        ))
        command = ["cc", "-shared", "-fPIC", "-Wl,-z,max-page-size=16384"]
        if stripped:
            command.append("-s")
        subprocess.run(command + [str(source), "-o", str(library)],
                       check=True, capture_output=True, text=True)
        return library.read_bytes()

    def test_every_native_declaration_is_required(self):
        activity = (Path(__file__).resolve().parents[1] / "platform/android/gradle/app/src/main"
                    / "java/com/flectar/mail/FlectarActivity.java")
        declared = set(re.findall(r"\bnative\s+\w+\s+(\w+)\s*\(", activity.read_text()))
        prefix = "Java_com_flectar_mail_FlectarActivity_"
        required = {name.removeprefix(prefix) for name in native.REQUIRED_EXPORTS
                    if name.startswith(prefix)}
        self.assertEqual(declared, required)

    def test_release_stripping_preserves_required_dynamic_exports(self):
        with tempfile.TemporaryDirectory() as directory:
            data = self.build_library(directory)
            native.verify_elf(data, "fixture.so")
            native.verify_jni_exports(data, "fixture.so")

    def test_each_missing_callback_and_entry_point_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            for name in sorted(native.REQUIRED_EXPORTS):
                with self.subTest(symbol=name):
                    data = self.build_library(directory, (name, ""))
                    with self.assertRaisesRegex(ValueError, name):
                        native.verify_jni_exports(data, "fixture.so")

    def test_non_callable_symbols_do_not_satisfy_the_contract(self):
        name = "Java_com_flectar_mail_FlectarActivity_nativeGoogleAuthorizationResult"
        variants = {
            "hidden": f'__attribute__((visibility("hidden"))) void {name}(void) {{}}',
            "local": f"static void {name}(void) {{}}",
            "undefined": f"extern void {name}(void); void call_missing(void) {{ {name}(); }}",
            "data": f"int {name} = 1;",
        }
        with tempfile.TemporaryDirectory() as directory:
            for kind, source in variants.items():
                with self.subTest(kind=kind):
                    # Retain debug/static symbols to ensure they cannot mask a
                    # missing callable symbol in the dynamic table.
                    data = self.build_library(directory, (name, source), stripped=False)
                    with self.assertRaisesRegex(ValueError, name):
                        native.verify_jni_exports(data, "fixture.so")

    def test_apk_checks_each_packaged_abi(self):
        with tempfile.TemporaryDirectory() as directory:
            complete = self.build_library(directory)
            name = "Java_com_flectar_mail_FlectarActivity_nativeGoogleAuthorizationResult"
            incomplete = self.build_library(directory, (name, ""))
            apk = Path(directory) / "fixture.apk"
            for second_library in (complete, incomplete):
                with zipfile.ZipFile(apk, "w") as archive:
                    for abi, app in (("arm64-v8a", complete), ("x86_64", second_library)):
                        # Host ELF fixtures represent each packaging location;
                        # they are never executed as Android code.
                        archive.writestr(f"lib/{abi}/libflectar_mail_android.so", app)
                        archive.writestr(f"lib/{abi}/libpdfium.so", complete)
                        notices = f"assets/licenses/PDFium/{abi}/pdfium-licenses"
                        archive.writestr(f"{notices}/build.json", "{}")
                        archive.writestr(f"{notices}/LICENSE", "fixture")
                if second_library == complete:
                    native.verify_apk(apk)
                else:
                    with self.assertRaisesRegex(ValueError, f"x86_64/.*{name}"):
                        native.verify_apk(apk)


if __name__ == "__main__":
    unittest.main()
