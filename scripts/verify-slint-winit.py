#!/usr/bin/env python3
"""Reconstruct the pinned Slint winit clipboard backport."""

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
PATCHES = ROOT / "patches/slint-winit"
CRATE = "i-slint-backend-winit"


def files(root):
    return {
        p.relative_to(root): p.read_bytes()
        for p in root.rglob("*")
        if p.is_file() and p.relative_to(root) != Path("Cargo.lock")
    }


def main():
    metadata = json.loads((PATCHES / "upstream.json").read_text())
    version = metadata["version"]
    url = f"https://static.crates.io/crates/{CRATE}/{CRATE}-{version}.crate"
    with urllib.request.urlopen(url, timeout=60) as response:
        archive = response.read()
    if hashlib.sha256(archive).hexdigest() != metadata["sha256"]:
        raise RuntimeError("Slint winit archive checksum differs from metadata")
    with tempfile.TemporaryDirectory(prefix="mail-slint-winit-") as tmp:
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
            raise RuntimeError("Slint release revision differs from metadata")
        for patch in sorted(PATCHES.glob("*.patch")):
            subprocess.run(["git", "apply", str(patch)], check=True, cwd=tree)
        expected = files(tree / "crates" / CRATE)
        actual = files(ROOT / "crates" / CRATE)
        for name in sorted(expected.keys() | actual.keys()):
            if expected.get(name) != actual.get(name):
                raise RuntimeError(f"Vendored source differs: crates/{CRATE}/{name}")

    for manifest, prefix in [("Cargo.toml", ""), ("platform/android/Cargo.toml", "../../")]:
        patches = tomllib.loads((ROOT / manifest).read_text())["patch"]["crates-io"]
        if patches.get(CRATE) != {"path": f"{prefix}crates/{CRATE}"}:
            raise RuntimeError(f"Missing Slint winit patch in {manifest}")
        lockfile = ROOT / Path(manifest).with_name("Cargo.lock")
        packages = tomllib.loads(lockfile.read_text())["package"]

        def resolved(name):
            return [p for p in packages if p["name"] == name]

        backend = resolved(CRATE)
        if len(backend) != 1 or backend[0]["version"] != version or "source" in backend[0]:
            raise RuntimeError(f"{lockfile}: {CRATE} must resolve to the local backport")
        if [p["version"] for p in resolved("slint")] != [version]:
            raise RuntimeError(f"{lockfile}: the backport targets Slint {version}")
        # smithay-clipboard 0.7.3 moved to SCTK 0.20; 0.7.2 shares winit's SCTK 0.19.
        if [p["version"] for p in resolved("smithay-clipboard")] != [metadata["smithay_clipboard"]]:
            raise RuntimeError(f"{lockfile}: smithay-clipboard must stay on SCTK 0.19")
        if len(resolved("smithay-client-toolkit")) != 1:
            raise RuntimeError(f"{lockfile}: expected a single smithay-client-toolkit")
    print("Slint winit source checksum, clipboard backport, manifests, and lockfiles verified.")


if __name__ == "__main__":
    main()
