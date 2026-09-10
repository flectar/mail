#!/usr/bin/env python3
"""Write the metadata for our embedded, signed iOS PDFium framework."""
import pathlib
import plistlib
import sys


def write(path, simulator):
    metadata = {
        "CFBundleDevelopmentRegion": "en",
        "CFBundleExecutable": "PDFium",
        "CFBundleIdentifier": "com.flectar.pdfium",
        "CFBundleInfoDictionaryVersion": "6.0",
        "CFBundleName": "PDFium",
        "CFBundlePackageType": "FMWK",
        "CFBundleShortVersionString": "8044.0.0",
        "CFBundleVersion": "8044",
        "CFBundleSupportedPlatforms": ["iPhoneSimulator" if simulator else "iPhoneOS"],
        "MinimumOSVersion": "17.0",
    }
    path.write_bytes(plistlib.dumps(metadata))


if __name__ == "__main__":
    write(pathlib.Path(sys.argv[1]), bool(int(sys.argv[2])))
