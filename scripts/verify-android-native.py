#!/usr/bin/env python3
"""Verify packaged native ABI completeness and 16 KiB ELF load alignment."""
import argparse
import struct
import zipfile


def verify_elf(data, name):
    if data[:6] != b"\x7fELF\x02\x01":
        raise ValueError(f"{name}: expected little-endian ELF64")
    offset = struct.unpack_from("<Q", data, 32)[0]
    entry_size, count = struct.unpack_from("<HH", data, 54)
    if entry_size < 56 or count == 0 or offset + count * entry_size > len(data):
        raise ValueError(f"{name}: invalid ELF program headers")
    loads = 0
    for i in range(count):
        entry = offset + i * entry_size
        if struct.unpack_from("<I", data, entry)[0] != 1:
            continue
        file_offset, address = struct.unpack_from("<QQ", data, entry + 8)
        alignment = struct.unpack_from("<Q", data, entry + 48)[0]
        if alignment < 16384 or file_offset % 16384 != address % 16384:
            raise ValueError(f"{name}: PT_LOAD is not aligned for 16 KiB Android pages")
        loads += 1
    if not loads:
        raise ValueError(f"{name}: missing ELF load segments")


def verify_apk(path):
    with zipfile.ZipFile(path) as archive:
        names = set(archive.namelist())
        app_libraries = sorted(name for name in names if name.endswith("/libflectar_mail_android.so"))
        if not app_libraries:
            raise ValueError("APK contains no Flectar native application")
        for app in app_libraries:
            abi = app.split("/")[1]
            pdfium = f"lib/{abi}/libpdfium.so"
            provenance = f"assets/licenses/PDFium/{abi}/pdfium-licenses/build.json"
            license_name = f"assets/licenses/PDFium/{abi}/pdfium-licenses/LICENSE"
            if not {pdfium, provenance, license_name} <= names:
                raise ValueError(f"{abi}: missing bundled PDFium runtime, provenance or license")
        for name in sorted(names):
            if name.startswith("lib/") and name.endswith(".so"):
                verify_elf(archive.read(name), name)
    print("PASS: native libraries include PDFium and licenses, with 16 KiB ELF alignment")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("apk")
    verify_apk(parser.parse_args().apk)
