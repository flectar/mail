#!/usr/bin/env python3
"""Stage desktop and mobile downloads with checksums."""

import hashlib
import importlib.util
import json
import os
import shutil
import sys
from datetime import datetime, timezone
from pathlib import Path


def _ios_release():
    sys.dont_write_bytecode = True
    spec = importlib.util.spec_from_file_location("ios_release", Path(__file__).with_name("ios-release.py"))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


ios_release = _ios_release()


def prepare(source: Path, destination: Path, version: str, repository: str, date: str) -> None:
    deb_version = version.replace("-", "~", 1)
    # GitHub rewrites '~' in asset filenames. Normalize before checksumming;
    # the package's internal Debian version keeps '~' for prerelease ordering.
    deb_filename_version = deb_version.replace("~", ".")
    # Each workflow artifact unpacks into source/<artifact name>.
    packages = (
        # (workflow artifact, package file, download name)
        ("flectar-mail-linux-x64", "flectar-mail.AppImage", f"flectar-mail-{version}-linux-x64.AppImage"),
        ("flectar-mail-linux-x64", f"flectar-mail_{deb_version}_amd64.deb", f"flectar-mail_{deb_filename_version}_amd64.deb"),
        ("flectar-mail-linux-arm64", "flectar-mail.AppImage", f"flectar-mail-{version}-linux-arm64.AppImage"),
        ("flectar-mail-linux-arm64", f"flectar-mail_{deb_version}_arm64.deb", f"flectar-mail_{deb_filename_version}_arm64.deb"),
        ("flectar-mail-rpm-x64", "flectar-mail.rpm", f"flectar-mail-{version}-linux-x64.rpm"),
        ("flectar-mail-rpm-arm64", "flectar-mail.rpm", f"flectar-mail-{version}-linux-arm64.rpm"),
        ("flectar-mail-flatpak-x64", "flectar-mail.flatpak", f"flectar-mail-{version}-linux-x64.flatpak"),
        ("flectar-mail-flatpak-arm64", "flectar-mail.flatpak", f"flectar-mail-{version}-linux-arm64.flatpak"),
        ("flectar-mail-windows-x64", "flectar-mail-windows-x64.zip", f"flectar-mail-{version}-windows-x64.zip"),
        ("flectar-mail-windows-x64", "flectar-mail-windows-x64-setup.exe", f"flectar-mail-{version}-windows-x64-setup.exe"),
        ("flectar-mail-windows-arm64", "flectar-mail-windows-arm64.zip", f"flectar-mail-{version}-windows-arm64.zip"),
        ("flectar-mail-windows-arm64", "flectar-mail-windows-arm64-setup.exe", f"flectar-mail-{version}-windows-arm64-setup.exe"),
        ("flectar-mail-macos-arm64", "flectar-mail-macos-arm64.zip", f"flectar-mail-{version}-macos-arm64.zip"),
        ("flectar-mail-macos-arm64", "flectar-mail-macos-arm64.dmg", f"flectar-mail-{version}-macos-arm64.dmg"),
        ("flectar-mail-macos-x64", "flectar-mail-macos-x64.zip", f"flectar-mail-{version}-macos-x64.zip"),
        ("flectar-mail-macos-x64", "flectar-mail-macos-x64.dmg", f"flectar-mail-{version}-macos-x64.dmg"),
        ("flectar-mail-ios-arm64", "flectar-mail-ios-arm64.ipa", ios_release.ipa_name(version)),
        ("flectar-mail-android-signed-arm64", "flectar-mail.apk", f"flectar-mail-{version}-android-arm64.apk"),
    )
    # Validate every input before copying anything; unrelated benchmark files
    # are intentionally excluded from the public downloads.
    selected = []
    for artifact, name, output_name in packages:
        matches = list((source / artifact).rglob(name))
        if len(matches) != 1 or not matches[0].is_file() or matches[0].stat().st_size == 0:
            raise ValueError(f"Expected exactly one nonempty {artifact}/{name}, found {len(matches)}")
        selected.append((matches[0], output_name))
    # AltStore and SideStore read the newest IPA's description from the
    # latest release, at a fixed address (scripts/ios-release.py).
    ipa = next(path for path, name in selected if name == ios_release.ipa_name(version))
    altstore_source = json.dumps(ios_release.source(ipa, version, repository, date), indent=2, ensure_ascii=False)
    destination.mkdir()  # Refuse to mix new artifacts with a stale dist folder.
    for path, name in selected:
        shutil.copyfile(path, destination / name)
    (destination / ios_release.SOURCE_NAME).write_text(altstore_source + "\n", encoding="utf-8")
    checksums = []
    for output in sorted(destination.iterdir()):
        with output.open("rb") as stream:
            digest = hashlib.file_digest(stream, "sha256").hexdigest()
        checksums.append(f"{digest}  {output.name}\n")
    (destination / "SHA256SUMS").write_text("".join(checksums), encoding="utf-8")


def release_notes(version: str) -> str:
    android = (
        "An Android arm64 APK (Android 8.0+) is included. "
        "Starting with alpha 8, previews and stable releases share one signing "
        "identity, so newer versions can update the app without uninstalling it. "
        "Alpha 6 and alpha 7 used different keys and cannot update to this identity. "
        "Android OAuth requires separate registrations tied to the APK signing "
        "identity; desktop keys alone do not enable it.\n"
    )
    return (
        "Desktop builds for Linux and Windows on x64 and arm64, and macOS Apple "
        "silicon and Intel. Linux testers can choose AppImage, Debian, Fedora "
        "RPM, or Flatpak on either architecture.\n\n"
        "The RPMs support Fedora 43 and 44 on x86_64 and aarch64. Each package is "
        "built on Fedora 43 and tested in clean containers on both releases, "
        "including PDF rendering and X11/Wayland startup. Other RPM distributions "
        "are not yet tested. Install one with `sudo dnf install ./flectar-mail-"
        f"{version}-linux-x64.rpm` (or `-linux-arm64.rpm`); if a graphical "
        "installer reports a conflict, this command shows the dependency details.\n\n"
        "Gmail and Outlook sign-in require a bundled OAuth registration or your own "
        "keys in **Sign-in settings** on the welcome screen. IMAP/SMTP and JMAP "
        "do not require Google or Microsoft app keys. Provider policies still apply.\n\n"
        "Windows downloads are unsigned. macOS downloads are ad-hoc signed, "
        "not Developer ID signed or notarized; OS security prompts are expected. "
        "macOS requires version 14 or later. Updates are installed manually.\n\n"
        "Download the installer for your platform, or use a ZIP for manual "
        "installation. The preview Flatpak is a sideloaded bundle and does not "
        "receive Flathub updates yet; close-to-tray is disabled in that package. "
        "SHA256SUMS covers every download, and GitHub "
        "records signed build-provenance attestations.\n\n"
        "The iOS app (iOS 17 or later) is unsigned. Sideload it with AltStore, "
        "SideStore, or Sideloadly, which sign it with your Apple ID; see "
        "https://github.com/flectar/mail/blob/main/docs/install.md#ios. In AltStore "
        "or SideStore, add the source "
        f"{ios_release.source_address('flectar/mail')} to receive updates.\n\n"
        + android
    )


if __name__ == "__main__":
    version = os.environ["RELEASE_VERSION"]
    published = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    prepare(Path("release-assets"), Path("dist"), version, os.environ["GITHUB_REPOSITORY"], published)
    Path("release-notes.md").write_text(release_notes(version), encoding="utf-8")
