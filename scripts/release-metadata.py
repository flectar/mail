#!/usr/bin/env python3
"""Validate release versions before starting native builds."""

import argparse
import os
import re
import tomllib
from pathlib import Path


def metadata(version: str, tag: str | None = None) -> dict[str, str]:
    # Keep native package versions predictable; add more channels deliberately.
    match = re.fullmatch(
        r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
        r"(?:-(alpha|beta|rc)\.([1-9][0-9]*))?",
        version,
    )
    if not match:
        raise ValueError("Use X.Y.Z or X.Y.Z-{alpha,beta,rc}.N in Cargo.toml")
    if tag is not None and tag != f"v{version}":
        raise ValueError(f"Tag {tag!r} must match Cargo.toml version: v{version}")
    return {
        "version": version,
        "native_version": ".".join(match.group(1, 2, 3)),
        "prerelease": str(match.group(4) is not None).lower(),
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag")
    args = parser.parse_args()
    with Path("Cargo.toml").open("rb") as manifest:
        version = tomllib.load(manifest)["package"]["version"]
    for path in (Path("Cargo.lock"), Path("platform/android/Cargo.lock")):
        with path.open("rb") as lockfile:
            packages = tomllib.load(lockfile)["package"]
        if not any(p["name"] == "flectar-mail" and p["version"] == version for p in packages):
            raise SystemExit(f"{path} does not match the root Cargo.toml; refresh its workspace lockfile")
    try:
        values = metadata(version, args.tag)
    except ValueError as error:
        raise SystemExit(str(error)) from error
    output = "".join(f"{key}={value}\n" for key, value in values.items())
    print(output, end="")
    if output_path := os.environ.get("GITHUB_OUTPUT"):
        with open(output_path, "a", encoding="utf-8") as stream:
            stream.write(output)


if __name__ == "__main__":
    main()
