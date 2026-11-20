#!/usr/bin/env python3
"""Stage Linux desktop and AppStream metadata for the Cargo package version and architecture."""

import argparse
import os
import re
import tomllib
import xml.etree.ElementTree as ET
from datetime import datetime, timezone
from pathlib import Path


# AppImage names for the architectures that release packages are built for.
ARCHITECTURES = ("x86_64", "aarch64")


def stage(project: Path, destination: Path, *, arch: str | None = None, flatpak: bool = False) -> str:
    with (project / "Cargo.toml").open("rb") as stream:
        version = tomllib.load(stream)["package"]["version"]
    if not re.fullmatch(r"\d+\.\d+\.\d+(?:-(?:alpha|beta|rc)\.[1-9]\d*)?", version):
        raise ValueError(f"Unsupported release version: {version}")
    resources = project / "resources"
    desktop = (resources / "com.flectar.mail.desktop").read_text(encoding="utf-8")
    desktop, count = re.subn(
        r"^X-AppImage-Version=.*$", f"X-AppImage-Version={version}", desktop,
        flags=re.MULTILINE,
    )
    if count != 1:
        raise ValueError("Expected one X-AppImage-Version field in the desktop template")
    if flatpak:
        desktop = re.sub(r"^X-AppImage-[^\n]*\n?", "", desktop, flags=re.MULTILINE)
    else:
        if arch not in ARCHITECTURES:
            raise ValueError(f"Expected an architecture from {', '.join(ARCHITECTURES)}, not {arch}")
        desktop, count = re.subn(
            r"^X-AppImage-Arch=.*$", f"X-AppImage-Arch={arch}", desktop, flags=re.MULTILINE,
        )
        if count != 1:
            raise ValueError("Expected one X-AppImage-Arch field in the desktop template")
    parser = ET.XMLParser(target=ET.TreeBuilder(insert_comments=True))
    metainfo = ET.parse(resources / "com.flectar.mail.metainfo.xml", parser=parser)
    releases = metainfo.find("./releases")
    if releases is None:
        raise ValueError("Missing releases in the AppStream template")
    # Published releases keep the date in the template. A build of a version
    # that is not listed yet, such as a pull request that bumps it, adds its
    # own entry dated by its source.
    release = next((r for r in releases.iterfind("release") if r.get("version") == version), None)
    if release is None:
        # Reproducible builds can supply an epoch; otherwise use the UTC build date.
        epoch = os.environ.get("SOURCE_DATE_EPOCH")
        release_date = (
            datetime.fromtimestamp(int(epoch), timezone.utc)
            if epoch is not None else datetime.now(timezone.utc)
        ).date().isoformat()
        release = ET.Element("release", {"version": version, "date": release_date})
        release.tail = releases.text
        releases.insert(0, release)
    release.set("type", "development" if "-" in version else "stable")
    desktop_path = destination / "usr/share/applications/com.flectar.mail.desktop"
    metainfo_path = destination / "usr/share/metainfo/com.flectar.mail.metainfo.xml"
    desktop_path.parent.mkdir(parents=True, exist_ok=True)
    metainfo_path.parent.mkdir(parents=True, exist_ok=True)
    desktop_path.write_text(desktop, encoding="utf-8")
    metainfo.write(metainfo_path, encoding="utf-8", xml_declaration=True)
    return version


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    target = parser.add_mutually_exclusive_group(required=True)
    target.add_argument("--arch", choices=ARCHITECTURES, help="architecture of the packaged executable")
    target.add_argument("--flatpak", action="store_true", help="omit AppImage-only desktop keys")
    parser.add_argument("destination", type=Path, help="AppDir or Debian package root")
    args = parser.parse_args()
    print(stage(Path(__file__).resolve().parent.parent, args.destination, arch=args.arch, flatpak=args.flatpak))
