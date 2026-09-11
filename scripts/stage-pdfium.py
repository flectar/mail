#!/usr/bin/env python3
"""Stage a checksum-pinned, non-V8 PDFium runtime for desktop and mobile app packaging.

No downloads happen at application startup or during cargo build. Release
packaging calls this explicitly; developers can target target/debug.
"""
import argparse
import hashlib
import io
import json
import pathlib
import shutil
import tarfile
import urllib.request

RELEASE = "chromium/8044"
ASSETS = {
    "linux-x64": ("eb142f416aed3a72fc5a02dbd5884868a16cb99dc0cf53e6bdd64afbf67b05f4", "lib/libpdfium.so"),
    "mac-arm64": ("61424884d4a7f153b808deba6437848e4400834ce30aaf95d3050da44df8f420", "lib/libpdfium.dylib"),
    "android-arm64": ("37686e64fa005484d619550a78805500de4c5a6f4df7aa06d8576145bfbee98f", "lib/libpdfium.so"),
    "android-x64": ("e7e3072b8ad6f36ef58db999c2d91a22fa165a904d69385f13db4537c233d736", "lib/libpdfium.so"),
    "ios-device-arm64": ("52be735d07b6c498ed8ce68b7f0cb5a209bbe61593c7a86c556756b80abbb938", "lib/libpdfium.dylib"),
    "ios-simulator-arm64": ("544537fce5257146e0df501c632d490d1aa95768737770f1912c8e23b1dd238d", "lib/libpdfium.dylib"),
    "ios-simulator-x64": ("8d4e33e415e29894aa98481bc9a9162a42039a1672796db14c9fed9cd545c881", "lib/libpdfium.dylib"),
    "win-x64": ("78a17d9a5f14467631c26a3ac8741b27a0471ecc05bd6a119b523598160a0537", "bin/pdfium.dll"),
}


def stage(target, destination, cache=None):
    checksum, library = ASSETS[target]
    url = f"https://github.com/bblanchon/pdfium-binaries/releases/download/{RELEASE}/pdfium-{target}.tgz"
    cached = cache / f"{checksum}.tgz" if cache else None
    if cached and cached.is_file():
        data = cached.read_bytes()
    else:
        with urllib.request.urlopen(url, timeout=60) as response:
            data = response.read(64 * 1024 * 1024 + 1)
    if hashlib.sha256(data).hexdigest() != checksum:
        raise ValueError("PDFium archive checksum mismatch")
    if cached and not cached.is_file():
        cache.mkdir(parents=True, exist_ok=True)
        # Concurrent Xcode architecture builds must never observe a partial cache.
        import tempfile
        with tempfile.NamedTemporaryFile(dir=cache, delete=False) as temp:
            temp.write(data)
            temporary = pathlib.Path(temp.name)
        temporary.replace(cached)
    destination.mkdir(parents=True, exist_ok=True)
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        members = {member.name.removeprefix("./"): member for member in archive.getmembers()}
        for name, member in members.items():
            if name != library and not name.startswith("licenses/") and name != "LICENSE":
                continue
            if not member.isfile() or ".." in pathlib.PurePosixPath(name).parts:
                raise ValueError("Invalid PDFium archive member")
            relative = pathlib.Path(library).name if name == library else pathlib.Path("pdfium-licenses") / name
            output = destination / relative
            output.parent.mkdir(parents=True, exist_ok=True)
            with archive.extractfile(member) as source, output.open("wb") as sink:
                shutil.copyfileobj(source, sink)
        if not (destination / pathlib.Path(library).name).is_file():
            raise ValueError("PDFium runtime missing from archive")
    (destination / "pdfium-licenses" / "build.json").write_text(json.dumps({"release": RELEASE, "sha256": checksum, "url": url}, indent=2) + "\n")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target", choices=ASSETS)
    parser.add_argument("destination", type=pathlib.Path)
    parser.add_argument("--cache", type=pathlib.Path, help="Verified archive cache for repeated packaging builds")
    args = parser.parse_args()
    stage(args.target, args.destination, args.cache)
