#!/usr/bin/env python3
"""Write the Flathub manifest and the sources it lists.

With --tag and --commit the manifest builds that release from GitHub, which is
what the com.flectar.mail repository on Flathub holds. With --local it builds
this checkout instead, so CI can build and lint the Flathub variant of a pull
request before any tag exists.
"""

import argparse
import re
import shutil
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TEMPLATE = ROOT / "packaging/flathub/com.flectar.mail.yml"
CARGO_SOURCES = ROOT / "packaging/flatpak/cargo-sources.json"
GIT_SOURCE = re.compile(
    r"^(?P<indent> +)- type: git\n"
    r"(?P=indent)  url: https://github\.com/flectar/mail\.git\n"
    r"(?P=indent)  tag: '@TAG@'\n"
    r"(?P=indent)  commit: '@COMMIT@'\n",
    re.MULTILINE,
)
# Kept out of the local source like the bundle manifest does.
LOCAL_SKIP = (
    ".artifacts", ".devcontainer", ".env", ".flatpak-builder", ".git", ".idea",
    ".Trash-*", ".vscode", "libs", "target", "tmp",
)


def manifest(*, tag: str | None = None, commit: str | None = None, local: Path | None = None) -> str:
    text = TEMPLATE.read_text(encoding="utf-8")
    match = GIT_SOURCE.search(text)
    if match is None:
        raise ValueError(f"{TEMPLATE} has no release source to fill in")
    indent = match.group("indent")
    if local is not None:
        lines = [f"{indent}- type: dir", f"{indent}  path: {local}", f"{indent}  skip:"]
        lines += [f"{indent}    - {name}" for name in LOCAL_SKIP]
        source = "\n".join(lines) + "\n"
    else:
        if not re.fullmatch(r"v\d+\.\d+\.\d+(?:-(?:alpha|beta|rc)\.[1-9]\d*)?", tag or ""):
            raise ValueError(f"Expected a release tag such as v0.1.0, not {tag!r}")
        if not re.fullmatch(r"[0-9a-f]{40}", commit or ""):
            raise ValueError("Expected the tag's full 40-character commit")
        source = match.group(0).replace("@TAG@", tag).replace("@COMMIT@", commit)
    return text[: match.start()] + source + text[match.end():]


def write(destination: Path, **options) -> None:
    destination.mkdir(parents=True, exist_ok=True)
    (destination / "com.flectar.mail.yml").write_text(manifest(**options), encoding="utf-8")
    shutil.copyfile(CARGO_SOURCES, destination / "cargo-sources.json")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=Path, help="directory for the manifest and cargo-sources.json")
    release = parser.add_argument_group("release")
    release.add_argument("--tag", help="release tag, such as v0.1.0")
    release.add_argument("--commit", help="the tag's full commit hash")
    parser.add_argument("--local", action="store_true", help="build this checkout instead of a release")
    args = parser.parse_args()
    if args.local == bool(args.tag or args.commit):
        parser.error("use either --local or --tag with --commit")
    if args.local:
        write(args.destination, local=ROOT)
    else:
        write(args.destination, tag=args.tag, commit=args.commit)
    print(args.destination / "com.flectar.mail.yml")
