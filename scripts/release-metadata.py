#!/usr/bin/env python3
"""Validate release versions before starting native builds."""

import argparse
import os
import re
import subprocess
import tomllib
import xml.etree.ElementTree as ET
from pathlib import Path

# The full name, so that a tag called origin/main cannot stand in for main.
MAIN = "refs/remotes/origin/main"


def msix_version(match: re.Match) -> str:
    """Microsoft Store package version, ordered like the Android version codes.

    The Store requires Major.Minor.Build.0 with a nonzero first part and parts
    up to 65535, so a 0.x version cannot keep its own numbers. Major is offset
    by one, Minor holds minor and patch, and Build orders the channels like
    scripts/android-version-code.py: alpha.N < beta.N < rc.N < the release.
    0.1.0-beta.1 becomes 1.100.1001.0 and 0.1.0 becomes 1.100.9999.0.
    """
    major, minor, patch = map(int, match.group(1, 2, 3))
    stage, number = match.group(4, 5)
    if major > 20 or minor > 99 or patch > 99 or (number and int(number) > 999):
        raise ValueError("Store versions support major 0–20, minor/patch 0–99, and prerelease 1–999")
    build = {"alpha": 0, "beta": 1000, "rc": 2000}.get(stage, 9999) + int(number or 0)
    return f"{major + 1}.{minor * 100 + patch}.{build}.0"


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
        "msix_version": msix_version(match),
        "prerelease": str(match.group(4) is not None).lower(),
    }


def listed_release(metainfo: Path, version: str) -> None:
    """Software centres show the AppStream release history, so a tagged
    release must already head it; packaging keeps the listed date."""
    release = ET.parse(metainfo).find("./releases/release")
    if release is None or release.get("version") != version:
        raise ValueError(f"Add a {version} release to the top of {metainfo} before tagging it")


def on_main(commit: str = "HEAD", main: str = MAIN, cwd: Path | None = None) -> None:
    """A release builds only a commit that main already holds, so a tag on
    any other commit cannot publish one. This needs main's whole history."""
    def git(*args: str) -> subprocess.CompletedProcess:
        return subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True)

    shallow = git("rev-parse", "--is-shallow-repository")
    if shallow.returncode != 0:
        raise ValueError(f"Could not read the git history: {shallow.stderr.strip()}")
    if shallow.stdout.strip() != "false":
        raise ValueError("Checking a release tag needs main's whole history. Fetch it without --depth.")
    if git("rev-parse", "--verify", "--quiet", f"{main}^{{commit}}").returncode != 0:
        raise ValueError(f"{main} is missing. Fetch main before checking a release tag.")
    result = git("merge-base", "--is-ancestor", commit, main)
    if result.returncode == 1:
        raise ValueError(f"The tagged commit is not on main ({main}). Tag a commit that main holds.")
    if result.returncode != 0:
        raise ValueError(f"Could not compare the tagged commit with main: {result.stderr.strip()}")


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
        if args.tag is not None:
            listed_release(Path("resources/com.flectar.mail.metainfo.xml"), version)
            on_main()
    except ValueError as error:
        raise SystemExit(str(error)) from error
    output = "".join(f"{key}={value}\n" for key, value in values.items())
    print(output, end="")
    if output_path := os.environ.get("GITHUB_OUTPUT"):
        with open(output_path, "a", encoding="utf-8") as stream:
            stream.write(output)


if __name__ == "__main__":
    main()
