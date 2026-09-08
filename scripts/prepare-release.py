#!/usr/bin/env python3
"""Stage desktop downloads and the prerelease Android APK with checksums."""

import hashlib
import os
import shutil
from pathlib import Path


def prepare(source: Path, destination: Path, version: str) -> None:
    deb_version = version.replace("-", "~", 1)
    packages = {
        "flectar-mail.AppImage": f"flectar-mail-{version}-linux-x64.AppImage",
        f"flectar-mail_{deb_version}_amd64.deb": f"flectar-mail_{deb_version}_amd64.deb",
        "flectar-mail-windows-x64.zip": f"flectar-mail-{version}-windows-x64.zip",
        "flectar-mail-windows-x64-setup.exe": f"flectar-mail-{version}-windows-x64-setup.exe",
        "flectar-mail-macos-arm64.zip": f"flectar-mail-{version}-macos-arm64.zip",
        "flectar-mail-macos-arm64.dmg": f"flectar-mail-{version}-macos-arm64.dmg",
    }
    if "-" in version:
        packages["flectar-mail.apk"] = f"flectar-mail-{version}-android-arm64-test.apk"
    # Validate every input before copying anything; unrelated benchmark files
    # are intentionally excluded from the public downloads.
    selected = []
    for name, output_name in packages.items():
        matches = list(source.rglob(name))
        if len(matches) != 1 or not matches[0].is_file() or matches[0].stat().st_size == 0:
            raise ValueError(f"Expected exactly one nonempty {name}, found {len(matches)}")
        selected.append((matches[0], output_name))
    destination.mkdir()  # Refuse to mix new artifacts with a stale dist folder.
    checksums = []
    for path, name in sorted(selected, key=lambda entry: entry[1]):
        output = destination / name
        shutil.copyfile(path, output)
        with output.open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
        checksums.append(f"{digest}  {name}\n")
    (destination / "SHA256SUMS").write_text("".join(checksums), encoding="utf-8")


def release_notes(version: str) -> str:
    android = (
        "An experimental Android arm64 test APK (Android 8.0+) is included. "
        "It is test-signed and is not a production or Play Store build. "
        "Updates require the same signing key; builds without a persistent test "
        "key may require uninstalling the previous app, which deletes local app data. "
        "Android OAuth requires separate registrations tied to the APK signing "
        "identity; desktop keys alone do not enable it.\n"
        if "-" in version else "The Android test APK is available in prereleases only.\n"
    )
    return (
        "Desktop builds for Linux x64, Windows x64, and macOS Apple silicon.\n\n"
        "Gmail and Outlook sign-in require a bundled OAuth registration or your own "
        "keys in **Sign-in settings** on the welcome screen. IMAP/SMTP and JMAP "
        "do not require Google or Microsoft app keys. Provider policies still apply.\n\n"
        "Windows downloads are unsigned. macOS downloads are ad-hoc signed, "
        "not Developer ID signed or notarized; OS security prompts are expected. "
        "macOS requires version 14 or later. Updates are installed manually.\n\n"
        "Download the installer for your platform, or use a ZIP for manual "
        "installation. SHA256SUMS covers every download. iOS archives are "
        "experimental and available through manual workflow runs only.\n\n"
        + android
    )


if __name__ == "__main__":
    version = os.environ["RELEASE_VERSION"]
    prepare(Path("release-assets"), Path("dist"), version)
    Path("release-notes.md").write_text(release_notes(version), encoding="utf-8")
