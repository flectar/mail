#!/usr/bin/env python3
"""Reconstruct the pinned jmap-client crate with its proxy and TLS options."""

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
PATCHES = ROOT / "patches/jmap-client"
CRATE = "jmap-client"


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
        raise RuntimeError("jmap-client archive checksum differs from metadata")
    with tempfile.TemporaryDirectory(prefix="mail-jmap-client-") as tmp:
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
            raise RuntimeError("jmap-client release revision differs from metadata")
        for patch in sorted(PATCHES.glob("*.patch")):
            subprocess.run(["git", "apply", str(patch)], check=True, cwd=tree)
        expected = files(tree / "crates" / CRATE)
        actual = files(ROOT / "crates" / CRATE)
        for name in sorted(expected.keys() | actual.keys()):
            if expected.get(name) != actual.get(name):
                raise RuntimeError(f"Vendored source differs: crates/{CRATE}/{name}")

    for manifest, prefix in [
        ("Cargo.toml", ""),
        ("platform/android/Cargo.toml", "../../"),
        ("platform/ohos/Cargo.toml", "../../"),
    ]:
        patches = tomllib.loads((ROOT / manifest).read_text())["patch"]["crates-io"]
        if patches.get(CRATE) != {"path": f"{prefix}crates/{CRATE}"}:
            raise RuntimeError(f"Missing jmap-client patch in {manifest}")
        lockfile = ROOT / Path(manifest).with_name("Cargo.lock")
        packages = tomllib.loads(lockfile.read_text())["package"]
        local = [p for p in packages if p["name"] == CRATE]
        if len(local) != 1 or local[0]["version"] != version or "source" in local[0]:
            raise RuntimeError(f"{lockfile}: {CRATE} must resolve to the local crate")
    print("jmap-client source checksum, patches, manifests, and lockfiles verified.")


if __name__ == "__main__":
    main()
