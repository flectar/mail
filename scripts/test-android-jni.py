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

ANDROID = Path(__file__).resolve().parents[1] / "platform/android"
APP_JAVA = ANDROID / "gradle/app/src/main/java/com/flectar/mail"
JAVA_PRIMITIVES = {
    "boolean": "Z", "byte": "B", "char": "C", "short": "S",
    "int": "I", "long": "J", "float": "F", "double": "D", "void": "V",
}
JAVA_STATEMENTS = {"if", "for", "while", "switch", "catch", "synchronized", "return", "new",
                   "throw", "else"}
JAVA_MODIFIERS = {"public", "protected", "private", "static", "final", "synchronized", "native"}


def java_descriptor(java_type, imports, app_classes):
    """The JNI descriptor of a type as a Java source file names it."""
    dimensions = java_type.count("[]") + java_type.count("...")
    name = java_type.replace("[]", "").replace("...", "").strip()
    if name in JAVA_PRIMITIVES:
        return "[" * dimensions + JAVA_PRIMITIVES[name]
    outer, _, inner = name.partition(".")
    if inner and outer[0].islower():
        # Named with its package.
        qualified = name
    elif outer in imports:
        qualified = imports[outer] + ("$" + inner.replace(".", "$") if inner else "")
    elif outer in app_classes:
        qualified = "com.flectar.mail." + name.replace(".", "$")
    else:
        # Java imports java.lang itself.
        qualified = "java.lang." + name
    return "[" * dimensions + "L" + qualified.replace(".", "/") + ";"


def java_methods():
    """The descriptor of each method the app's Java declares, with whether it is static."""
    sources = {source.stem: source.read_text() for source in APP_JAVA.glob("*.java")}
    methods = {}
    for text in sources.values():
        imports = {name: path for path, name in re.findall(r"^import\s+([\w.]+\.(\w+));", text, re.M)}
        # Descriptors leave out type arguments.
        while re.search(r"<[\w.?, \[\]]*>", text):
            text = re.sub(r"<[\w.?, \[\]]*>", "", text)
        for modifiers, returns, name, parameters in re.findall(
                rf"^[ \t]*((?:(?:{'|'.join(JAVA_MODIFIERS)})\s+)*)"
                r"([\w.$\[\]]+)\s+(\w+)\s*\(([^)]*)\)", text, re.M):
            if returns in JAVA_STATEMENTS | JAVA_MODIFIERS:
                continue
            types = [re.sub(r"\s+\w+$", "", re.sub(r"@\w+\s+|\bfinal\s+", "", parameter.strip()))
                     for parameter in parameters.split(",") if parameter.strip()]
            descriptor = ("(" + "".join(java_descriptor(t, imports, sources) for t in types) + ")"
                          + java_descriptor(returns, imports, sources))
            methods.setdefault(name, set()).add((descriptor, "static" in modifiers.split()))
    return methods


def rust_calls_into_java():
    """Each method the Android crate calls by name, with its descriptor and whether it is static."""
    calls = []
    for source in sorted((ANDROID / "src").glob("*.rs")):
        # A backslash at the end of a line continues a Rust string without the line break.
        text = re.sub(r"\\\n\s*", "", source.read_text())
        for match in re.finditer(r'"(\w+)",\s*"(\([^"]*\)[^"]*)"', text):
            before = text[:match.start()]
            static = before.rfind("call_static_method(") > before.rfind("call_method(")
            calls.append((source.name, match[1], match[2], static))
        # Helpers that take the method's name, as the settings ones do.
        for helper, descriptor in re.findall(
                r'fn (\w+)\(&self, class: &str, method: &str\)[^}]*?'
                r'call_static_method\(\s*class,\s*method,\s*"([^"]+)"', text, re.S):
            for name in re.findall(rf'\.{helper}\(\w+, "(\w+)"\)', text):
                calls.append((source.name, name, descriptor, True))
    return calls


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
        declared = {
            f"Java_com_flectar_mail_{source.stem}_{method}"
            for source in APP_JAVA.glob("*.java")
            for method in re.findall(r"\bnative\s+\w+\s+(\w+)\s*\(", source.read_text())
        }
        required = {name for name in native.REQUIRED_EXPORTS if name.startswith("Java_")}
        self.assertEqual(declared, required)

    def test_rust_calls_find_their_java_methods(self):
        # PR checks don't build the app, and ART reports a method it can't
        # find only when a phone makes the call.
        java = java_methods()
        sources = "\n".join(source.read_text() for source in APP_JAVA.glob("*.java"))
        declared = set(re.findall(r"\b(\w+)\s*\([^;{)]*\)\s*(?:throws\s+[\w.,\s]+)?\{",
                                  sources)) - JAVA_STATEMENTS
        checked = set()
        for source, name, descriptor, static in rust_calls_into_java():
            if name not in declared:
                continue  # an Android or Java method
            with self.subTest(call=f"{source}: {name}{descriptor}"):
                self.assertIn((descriptor, static), java.get(name, set()))
            checked.add(name)
        # Calls in strings across lines, through the settings helper and in
        # every file are all read.
        self.assertLessEqual({"show", "openSettings", "authorize", "chooseDocument"}, checked)

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
