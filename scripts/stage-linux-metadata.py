#!/usr/bin/env python3
"""Stage Linux desktop and AppStream metadata using the Cargo package version."""

import argparse
import os
import re
import tomllib
import xml.etree.ElementTree as ET
from datetime import datetime, timezone
from pathlib import Path


def stage(project: Path, destination: Path, *, flatpak: bool = False) -> str:
    with (project / "Cargo.toml").open("rb") as stream:
        version = tomllib.load(stream)["package"]["version"]
    if not re.fullmatch(r"\d+\.\d+\.\d+(?:-(?:alpha|beta|rc)\.[1-9]\d*)?", version):
        raise ValueError(f"Unsupported release version: {version}")
    # Reproducible builds can supply an epoch; otherwise use the UTC build date.
    epoch = os.environ.get("SOURCE_DATE_EPOCH")
    release_date = (
        datetime.fromtimestamp(int(epoch), timezone.utc)
        if epoch is not None else datetime.now(timezone.utc)
    ).date().isoformat()
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
    parser = ET.XMLParser(target=ET.TreeBuilder(insert_comments=True))
    metainfo = ET.parse(resources / "com.flectar.mail.metainfo.xml", parser=parser)
    # The first release describes the build being packaged. Retain older entries.
    release = metainfo.find("./releases/release")
    if release is None:
        raise ValueError("Missing current release in the AppStream template")
    release.set("version", version)
    release.set("date", release_date)
    release.set("type", "development" if "-" in version else "stable")
    release.attrib.pop("timestamp", None)
    desktop_path = destination / "usr/share/applications/com.flectar.mail.desktop"
    metainfo_path = destination / "usr/share/metainfo/com.flectar.mail.metainfo.xml"
    desktop_path.parent.mkdir(parents=True, exist_ok=True)
    metainfo_path.parent.mkdir(parents=True, exist_ok=True)
    desktop_path.write_text(desktop, encoding="utf-8")
    metainfo.write(metainfo_path, encoding="utf-8", xml_declaration=True)
    return version


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--flatpak", action="store_true", help="omit AppImage-only desktop keys")
    parser.add_argument("destination", type=Path, help="AppDir or Debian package root")
    args = parser.parse_args()
    print(stage(Path(__file__).resolve().parent.parent, args.destination, flatpak=args.flatpak))
