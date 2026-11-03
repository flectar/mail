#!/usr/bin/env python3
"""Package the iOS app for sideloading.

`ipa` turns the unsigned app that scripts/package-ios.sh builds into the IPA
that GitHub releases publish.
"""

import argparse
import os
import plistlib
import shutil
import stat
import time
import zipfile
from pathlib import Path, PurePosixPath

BUNDLE_ID = "com.flectar.mail"
# The earliest time a ZIP entry can record.
ZIP_EPOCH = 315532800


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
                with path.open("rb") as source, ipa.open(entry, "w") as target:
                    shutil.copyfileobj(source, target)
        partial.replace(output)
    finally:
        partial.unlink(missing_ok=True)


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
