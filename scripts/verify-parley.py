#!/usr/bin/env python3
"""Reconstruct the pinned Parley backport and compare it with checked-in sources."""

import hashlib
import io
import json
from pathlib import Path
import subprocess
import tarfile
import tempfile
import urllib.request

ROOT = Path(__file__).resolve().parent.parent
PATCHES = ROOT / "patches/parley"


def download(url, checksum):
    with urllib.request.urlopen(url, timeout=60) as response:
        data = response.read()
    actual = hashlib.sha256(data).hexdigest()
    if actual != checksum:
        raise RuntimeError(f"Checksum mismatch for {url}: {actual}")
    return data


def files(root):
    return {p.relative_to(root): p.read_bytes() for p in root.rglob("*") if p.is_file()}


def main():
    metadata = json.loads((PATCHES / "upstream.json").read_text())
    with tempfile.TemporaryDirectory(prefix="mail-parley-") as tmp:
        tree = Path(tmp)
        version = metadata["version"]
        archive = download(
            f"https://static.crates.io/crates/parley/parley-{version}.crate",
            metadata["sha256"],
        )
        with tarfile.open(fileobj=io.BytesIO(archive)) as source:
            prefix = f"parley-{version}/"
            for member in source.getmembers():
                if not member.isfile():
                    raise RuntimeError(f"Unexpected archive member: {member.name}")
                if not member.name.startswith(prefix) or ".." in Path(member.name).parts:
                    raise RuntimeError(f"Unsafe archive member: {member.name}")
                dest = tree / "crates/parley" / member.name[len(prefix):]
                dest.parent.mkdir(parents=True, exist_ok=True)
                dest.write_bytes(source.extractfile(member).read())
        provenance = json.loads((tree / "crates/parley/.cargo_vcs_info.json").read_text())
        if provenance["git"]["sha1"] != metadata["revision"]:
            raise RuntimeError("Parley release revision differs from metadata")
        revision = metadata["emoji_revision"]
        for name, checksum in metadata["emoji_files"].items():
            data = download(
                f"https://raw.githubusercontent.com/linebender/parley/{revision}/parley_emoji/{name}",
                checksum,
            )
            dest = tree / "crates/parley_emoji" / name
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_bytes(data)
        subprocess.run(
            ["git", "apply", str(PATCHES / "0001-emoji-presentation.patch")],
            check=True, cwd=tree,
        )
        for crate in ("parley", "parley_emoji"):
            expected = files(tree / "crates" / crate)
            actual = files(ROOT / "crates" / crate)
            for name in sorted(expected.keys() | actual.keys()):
                if expected.get(name) != actual.get(name):
                    raise RuntimeError(f"Vendored source differs: crates/{crate}/{name}")

    for manifest, prefix in [("Cargo.toml", ""), ("platform/android/Cargo.toml", "../../")]:
        text = (ROOT / manifest).read_text()
        patch_section = text.split("[patch.crates-io]", 1)[1].split("\n[", 1)[0]
        if f'parley = {{ path = "{prefix}crates/parley" }}' not in patch_section:
            raise RuntimeError(f"Missing Parley patch in {manifest}")
        lockfile = ROOT / Path(manifest).with_name("Cargo.lock")
        entries = lockfile.read_text().split("[[package]]")
        for crate in ("parley", "parley_emoji"):
            matches = [
                entry for entry in entries
                if f'name = "{crate}"\nversion = "{version}"' in entry
            ]
            if len(matches) != 1 or "source = " in matches[0]:
                raise RuntimeError(f"{lockfile}: {crate} must resolve to the local backport")
    if (ROOT / "LICENSES/Parley-Emoji-MIT.txt").read_bytes() != (
        ROOT / "crates/parley_emoji/LICENSE-MIT"
    ).read_bytes():
        raise RuntimeError("Packaged Parley emoji license differs from upstream")
    print("Parley backport, source checksums, manifests, and license verified.")


if __name__ == "__main__":
    main()
