#!/usr/bin/env python3
"""Rebuild crates/spellbook from the pinned crates.io release and its patch queue."""

import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import subprocess
import tarfile
import tempfile
import urllib.request

ROOT = Path(__file__).resolve().parent.parent
PATCHES = ROOT / "patches/spellbook"
VENDORED = ROOT / "crates/spellbook"


def download(url, checksum):
    request = urllib.request.Request(url, headers={"User-Agent": "flectar-mail-ci (+https://github.com/flectar/mail)"})
    with urllib.request.urlopen(request, timeout=60) as response:
        data = response.read()
    actual = hashlib.sha256(data).hexdigest()
    if actual != checksum:
        raise RuntimeError(f"Checksum mismatch for {url}: {actual}")
    return data


def excluded(name, patterns):
    return any(name == p or (p.endswith("/") and name.startswith(p)) for p in patterns)


def files(root):
    return {p.relative_to(root).as_posix(): p.read_bytes() for p in root.rglob("*") if p.is_file()}


def main():
    metadata = json.loads((PATCHES / "upstream.json").read_text())
    version = metadata["version"]
    archive = download(
        f"https://static.crates.io/crates/spellbook/spellbook-{version}.crate",
        metadata["sha256"],
    )
    with tempfile.TemporaryDirectory(prefix="mail-spellbook-") as tmp:
        tree = Path(tmp) / "crates/spellbook"
        prefix = f"spellbook-{version}/"
        with tarfile.open(fileobj=io.BytesIO(archive)) as source:
            for member in source.getmembers():
                if member.isdir():
                    continue
                if not member.isfile():
                    raise RuntimeError(f"Unexpected archive member: {member.name}")
                parts = PurePosixPath(member.name).parts
                if not member.name.startswith(prefix) or ".." in parts:
                    raise RuntimeError(f"Unsafe archive member: {member.name}")
                name = member.name[len(prefix):]
                if excluded(name, metadata["excluded"]):
                    continue
                dest = tree / name
                dest.parent.mkdir(parents=True, exist_ok=True)
                dest.write_bytes(source.extractfile(member).read())
        provenance = json.loads((tree / ".cargo_vcs_info.json").read_text())
        if provenance["git"]["sha1"] != metadata["revision"]:
            raise RuntimeError("spellbook release revision differs from metadata")
        for patch in metadata["patches"]:
            subprocess.run(["git", "apply", "--directory=crates/spellbook", str(PATCHES / patch)], check=True, cwd=tmp)
        expected = files(tree)
        actual = files(VENDORED)
        for name in sorted(expected.keys() | actual.keys()):
            if expected.get(name) != actual.get(name):
                raise RuntimeError(f"Vendored source differs: crates/spellbook/{name}")
    print(f"crates/spellbook matches spellbook {version} with {len(metadata['patches'])} patch(es)")


if __name__ == "__main__":
    main()
