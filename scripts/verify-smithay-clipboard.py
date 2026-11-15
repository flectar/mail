#!/usr/bin/env python3
"""Reconstruct the pinned smithay-clipboard with its file drop patch."""

import hashlib
import io
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile
import tomllib
import urllib.request

ROOT = Path(__file__).resolve().parent.parent
PATCHES = ROOT / "patches/smithay-clipboard"
CRATE = "smithay-clipboard"


def files(root):
    return {p.relative_to(root): p.read_bytes() for p in root.rglob("*") if p.is_file()}


def main():
    metadata = json.loads((PATCHES / "upstream.json").read_text())
    version = metadata["version"]
    url = f"https://static.crates.io/crates/{CRATE}/{CRATE}-{version}.crate"
    with urllib.request.urlopen(url, timeout=60) as response:
        archive = response.read()
    if hashlib.sha256(archive).hexdigest() != metadata["sha256"]:
        raise RuntimeError("smithay-clipboard archive checksum differs from metadata")
    with tempfile.TemporaryDirectory(prefix="mail-smithay-clipboard-") as tmp:
        tree = Path(tmp)
        with tarfile.open(fileobj=io.BytesIO(archive)) as source:
            prefix = f"{CRATE}-{version}/"
            for member in source.getmembers():
                if (not member.isfile() or not member.name.startswith(prefix)
                        or ".." in Path(member.name).parts):
                    raise RuntimeError(f"Unsafe archive member: {member.name}")
                dest = tree / "crates" / CRATE / member.name[len(prefix):]
                dest.parent.mkdir(parents=True, exist_ok=True)
                dest.write_bytes(source.extractfile(member).read())
        provenance = json.loads((tree / "crates" / CRATE / ".cargo_vcs_info.json").read_text())
        if provenance["git"]["sha1"] != metadata["revision"]:
            raise RuntimeError("smithay-clipboard release revision differs from metadata")
        for patch in sorted(PATCHES.glob("*.patch")):
            subprocess.run(["git", "apply", str(patch)], check=True, cwd=tree)
        expected = files(tree / "crates" / CRATE)
        actual = files(ROOT / "crates" / CRATE)
        for name in sorted(expected.keys() | actual.keys()):
            if expected.get(name) != actual.get(name):
                raise RuntimeError(f"Vendored source differs: crates/{CRATE}/{name}")

    # Every workspace that patches Slint's winit backend resolves its clipboard
    # here too, so the backend always builds against the patched crate.
    for manifest, prefix in [
        ("Cargo.toml", ""),
        ("platform/android/Cargo.toml", "../../"),
        ("platform/ohos/Cargo.toml", "../../"),
    ]:
        patches = tomllib.loads((ROOT / manifest).read_text())["patch"]["crates-io"]
        if patches.get(CRATE) != {"path": f"{prefix}crates/{CRATE}"}:
            raise RuntimeError(f"Missing smithay-clipboard patch in {manifest}")
        lockfile = ROOT / Path(manifest).with_name("Cargo.lock")
        packages = tomllib.loads(lockfile.read_text())["package"]
        clipboard = [p for p in packages if p["name"] == CRATE]
        if len(clipboard) != 1 or clipboard[0]["version"] != version or "source" in clipboard[0]:
            raise RuntimeError(f"{lockfile}: {CRATE} must resolve to the local patched crate")
    print("smithay-clipboard source checksum, file drop patch, manifests, and lockfiles verified.")


if __name__ == "__main__":
    main()
