#!/usr/bin/env python3
"""Package the iOS app for sideloading and describe it for AltStore.

`ipa` turns the unsigned app that scripts/package-ios.sh builds into the IPA
that GitHub releases publish. scripts/prepare-release.py adds the AltStore
source for it, which SideStore also reads, to the same release.
"""

import argparse
import hashlib
import importlib.util
import os
import plistlib
import re
import shutil
import stat
import struct
import sys
import time
import xml.etree.ElementTree as ET
import zipfile
from pathlib import Path, PurePosixPath

ROOT = Path(__file__).resolve().parent.parent
BUNDLE_ID = "com.flectar.mail"
# The earliest time a ZIP entry can record.
ZIP_EPOCH = 315532800
# People add the latest release's copy to AltStore or SideStore once, and the
# apps read it again from that address to find updates.
SOURCE_NAME = "flectar-mail-altstore-source.json"
# Info.plist keys that make iOS ask the user for a permission. AltStore and
# SideStore refuse apps that ask for one their source does not list.
PRIVACY_KEY = re.compile(r"NS\w+UsageDescription")
# The middle of the app icon's gradient.
TINT_COLOR = "#FE660C"
ICON = "platform/ios/Assets.xcassets/AppIcon.appiconset/AppIcon-1024.png"
SCREENSHOTS = {
    "iphone": ("mobile-light.png", "mobile-dark.png", "mobile-agenda-light.png"),
    "ipad": ("tablet-agenda-light.png",),
}


def _version_code():
    sys.dont_write_bytecode = True
    spec = importlib.util.spec_from_file_location("android_version_code", ROOT / "scripts/android-version-code.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.version_code


# CFBundleVersion, which scripts/package-ios.sh takes from the Android sequence.
version_code = _version_code()


def check_app(app: Path, short_version: str, build_version: str) -> list[Path]:
    """Check the app's identity and that it is unsigned; return its contents."""
    info = plistlib.loads((app / "Info.plist").read_bytes())
    for key, expected in (
        ("CFBundleIdentifier", BUNDLE_ID),
        ("CFBundleShortVersionString", short_version),
        ("CFBundleVersion", build_version),
    ):
        if info.get(key) != expected:
            raise ValueError(f"{key} is {info.get(key)!r}, expected {expected!r}")
    if not info.get("MinimumOSVersion"):
        raise ValueError("Info.plist names no MinimumOSVersion")
    executable = app / info.get("CFBundleExecutable", "")
    if executable == app or not executable.is_file() or not executable.stat().st_mode & stat.S_IXUSR:
        raise ValueError("The app's CFBundleExecutable is missing or not executable")
    contents = sorted(app.rglob("*"))
    for path in contents:
        if path.is_symlink():
            raise ValueError(f"{path.relative_to(app)} is a symbolic link")
        # Sideloading tools sign the app with the user's Apple ID. Anything
        # signed here would need its entitlements declared in the AltStore
        # source.
        if path.name in ("_CodeSignature", "embedded.mobileprovision"):
            raise ValueError(f"{path.relative_to(app)} is signing material; the IPA must be unsigned")
    return contents


def write_ipa(app: Path, output: Path, short_version: str, build_version: str) -> None:
    contents = check_app(app, short_version, build_version)
    # Entries keep their permissions but share one timestamp, so the same app
    # always packages into the same IPA.
    epoch = max(int(os.environ.get("SOURCE_DATE_EPOCH") or ZIP_EPOCH), ZIP_EPOCH)
    date_time = time.gmtime(epoch)[:6]
    partial = output.with_name(output.name + ".partial")
    try:
        with zipfile.ZipFile(partial, "w", zipfile.ZIP_DEFLATED) as ipa:
            payload = zipfile.ZipInfo("Payload/", date_time)
            payload.external_attr = (stat.S_IFDIR | 0o755) << 16 | 0x10
            ipa.writestr(payload, b"")
            for path in (app, *contents):
                name = PurePosixPath("Payload", app.name, path.relative_to(app).as_posix())
                entry = zipfile.ZipInfo.from_file(path, str(name), strict_timestamps=False)
                entry.date_time = date_time
                if path.is_dir():
                    ipa.writestr(entry, b"")
                    continue
                entry.compress_type = zipfile.ZIP_DEFLATED
                with path.open("rb") as data, ipa.open(entry, "w") as target:
                    shutil.copyfileobj(data, target)
        partial.replace(output)
    finally:
        partial.unlink(missing_ok=True)


def ipa_name(version: str) -> str:
    return f"flectar-mail-{version}-ios-arm64.ipa"


def source_address(repository: str) -> str:
    return f"https://github.com/{repository}/releases/latest/download/{SOURCE_NAME}"


def app_info(ipa: Path) -> dict:
    with zipfile.ZipFile(ipa) as archive:
        names = [name for name in archive.namelist() if re.fullmatch(r"Payload/[^/]+\.app/Info\.plist", name)]
        if len(names) != 1:
            raise ValueError(f"{ipa.name} must hold exactly one app")
        return plistlib.loads(archive.read(names[0]))


def png_size(path: Path) -> tuple[int, int]:
    header = path.read_bytes()[:24]
    if header[:8] != b"\x89PNG\r\n\x1a\n" or header[12:16] != b"IHDR":
        raise ValueError(f"{path.name} is not a PNG image")
    return struct.unpack(">II", header[16:24])


def describe() -> tuple[str, str]:
    """The app's summary and description, as Linux software centres show them."""
    component = ET.parse(ROOT / "resources/com.flectar.mail.metainfo.xml").getroot()
    summary = " ".join(component.findtext("summary").split())
    paragraphs = [" ".join("".join(p.itertext()).split()) for p in component.iterfind("description/p")]
    return summary, "\n\n".join(paragraphs)


def source(ipa: Path, version: str, repository: str, date: str) -> dict:
    """An AltStore source offering the IPA of release `version`.

    Its fields follow https://faq.altstore.io/developers/make-a-source. The
    version is repeated in the app's own fields for AltStore 1 and SideStore
    releases that predate version lists. Images come from the release's tag,
    so they never change under it.
    """
    info = app_info(ipa)
    for key, expected in (
        ("CFBundleIdentifier", BUNDLE_ID),
        ("CFBundleShortVersionString", version.split("-")[0]),
        ("CFBundleVersion", str(version_code(version))),
    ):
        if info.get(key) != expected:
            raise ValueError(f"The IPA of {version} has {key} {info.get(key)!r}, expected {expected!r}")
    with ipa.open("rb") as stream:
        sha256 = hashlib.file_digest(stream, "sha256").hexdigest()
    files = f"https://raw.githubusercontent.com/{repository}/v{version}"
    summary, description = describe()
    notes = f"Release notes: https://github.com/{repository}/releases/tag/v{version}"
    latest = {
        "version": info["CFBundleShortVersionString"],
        "buildVersion": info["CFBundleVersion"],
        "marketingVersion": version,
        "date": date,
        "localizedDescription": notes,
        "downloadURL": f"https://github.com/{repository}/releases/download/v{version}/{ipa_name(version)}",
        "size": ipa.stat().st_size,
        "sha256": sha256,
        "minOSVersion": info["MinimumOSVersion"],
    }
    screenshots = {}
    for device, names in SCREENSHOTS.items():
        screenshots[device] = []
        for name in names:
            # AltStore needs the size of iPad screenshots and uses it for the rest.
            width, height = png_size(ROOT / "resources/screenshots" / name)
            screenshots[device].append({"imageURL": f"{files}/resources/screenshots/{name}", "width": width, "height": height})
    app = {
        "name": "Flectar Mail",
        "bundleIdentifier": BUNDLE_ID,
        "developerName": "Flectar",
        "subtitle": summary,
        "localizedDescription": f"{description}\n\nThis is a preview. Report problems at https://github.com/{repository}/issues.",
        "iconURL": f"{files}/{ICON}",
        "tintColor": TINT_COLOR,
        "category": "utilities",
        "screenshots": screenshots,
        "versions": [latest],
        # The IPA is unsigned, so it declares no entitlements of its own.
        "appPermissions": {
            "entitlements": [],
            "privacy": {key: value for key, value in sorted(info.items()) if PRIVACY_KEY.fullmatch(key)},
        },
        "version": latest["version"],
        "versionDate": date,
        "versionDescription": notes,
        "downloadURL": latest["downloadURL"],
        "size": latest["size"],
        "screenshotURLs": [screenshot["imageURL"] for screenshot in screenshots["iphone"]],
    }
    return {
        "name": "Flectar Mail",
        "identifier": "com.flectar.mail.source",
        "sourceURL": source_address(repository),
        "subtitle": summary,
        "description": description,
        "iconURL": app["iconURL"],
        "website": "https://flectar.com/mail/",
        "tintColor": TINT_COLOR,
        "apps": [app],
        "news": [],
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    ipa = commands.add_parser("ipa", help="package the unsigned app as an IPA")
    ipa.add_argument("app", type=Path)
    ipa.add_argument("output", type=Path)
    ipa.add_argument("--short-version", required=True)
    ipa.add_argument("--build-version", required=True)
    args = parser.parse_args()
    try:
        write_ipa(args.app, args.output, args.short_version, args.build_version)
    except (OSError, ValueError) as error:
        raise SystemExit(str(error)) from error


if __name__ == "__main__":
    main()
