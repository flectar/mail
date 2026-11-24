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

RELEASE = "chromium/8179"
ASSETS = {
    "linux-x64": ("b4766a6bf2856efbcc6c3cc7d6ae9022bdc6bb36ddc1acdd92d1f1873f773f4b", "lib/libpdfium.so"),
    "linux-arm64": ("18bb6d568257b28d14f6c3b8bfb48caf73d4615b319ca4d43b424c2a03431d9f", "lib/libpdfium.so"),
    "mac-arm64": ("44b7c6f55dd33454d0909728bd0adf6c86a8e141b36f07a6b6f50e228e1dfb46", "lib/libpdfium.dylib"),
    "mac-x64": ("b29bbfe37efcad5b443fa3556c0485ec96fc1992d5b19d84df49d48fbed1d0d2", "lib/libpdfium.dylib"),
    "android-arm64": ("e74f8d3deb968dcf7e09a15152f19393df932ffc4e519149e61e8e0ee15c9753", "lib/libpdfium.so"),
    "android-x64": ("fd7f3afdae094549e77b44a95e88ff00cb59e84ba5eb5fc79470ccf10c85ccf9", "lib/libpdfium.so"),
    "ios-device-arm64": ("eb1687fd3398dd1632682915c41c1d0783dfc1d2ac513da63493c2e0d046402e", "lib/libpdfium.dylib"),
    "ios-simulator-arm64": ("e2fe1160057ea1cf3af75562babdd270f67e5e2bc68e5fbe5edc1f8fbc3b2473", "lib/libpdfium.dylib"),
    "ios-simulator-x64": ("4af23ca931ef1b742ca43b4aafb6a809d3b9aebf3304c54a5ef8215c5d4bbfb7", "lib/libpdfium.dylib"),
    "win-x64": ("836a77c9053f624839c66010cddea7b675628a36b8831d2330178bb169d241b6", "bin/pdfium.dll"),
    "win-arm64": ("9c659bf0cd9bdefcbf7ec0e4b1675c2961e0fdfd4ce097b411efd9742832fe06", "bin/pdfium.dll"),
}


def stage(target, destination, cache=None, licenses_destination=None):
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
    licenses_destination = licenses_destination or destination / "pdfium-licenses"
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:gz") as archive:
        members = {member.name.removeprefix("./"): member for member in archive.getmembers()}
        for name, member in members.items():
            if name != library and not name.startswith("licenses/") and name != "LICENSE":
                continue
            if not member.isfile() or ".." in pathlib.PurePosixPath(name).parts:
                raise ValueError("Invalid PDFium archive member")
            output = (
                destination / pathlib.Path(library).name
                if name == library
                else licenses_destination / name
            )
            output.parent.mkdir(parents=True, exist_ok=True)
            with archive.extractfile(member) as source, output.open("wb") as sink:
                shutil.copyfileobj(source, sink)
        if not (destination / pathlib.Path(library).name).is_file():
            raise ValueError("PDFium runtime missing from archive")
    licenses_destination.mkdir(parents=True, exist_ok=True)
    (licenses_destination / "build.json").write_text(json.dumps({"release": RELEASE, "sha256": checksum, "url": url}, indent=2) + "\n")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target", choices=ASSETS)
    parser.add_argument("destination", type=pathlib.Path)
    parser.add_argument("--cache", type=pathlib.Path, help="Verified archive cache for repeated packaging builds")
    parser.add_argument(
        "--licenses-destination",
        type=pathlib.Path,
        help="place PDFium notices outside the runtime-library directory",
    )
    args = parser.parse_args()
    stage(args.target, args.destination, args.cache, args.licenses_destination)
