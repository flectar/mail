#!/usr/bin/env python3
"""Regenerates packaging/flatpak/cargo-sources.json from Cargo.lock.

The Flatpak builds offline, so flatpak-builder downloads every crate first. A
crates.io package is one archive. A git dependency is one checkout of its
repository, from which each crate is copied into the vendor directory with its
workspace settings written out. This is what flatpak-builder-tools'
flatpak-cargo-generator does, and its output format is kept, so either tool
can regenerate the file.

Git dependencies are read from bare clones under
$XDG_CACHE_HOME/flectar-flatpak-git, which this script creates and updates.

Usage: scripts/update-flatpak-cargo-sources.py [--check]
"""

import argparse
import copy
import json
import os
import pathlib
import re
import subprocess
import sys
import tomllib
from typing import Any
from urllib.parse import parse_qs, urlparse

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUTPUT = ROOT / "packaging/flatpak/cargo-sources.json"
CRATES_IO = "https://static.crates.io/crates"
CARGO_HOME = "cargo"
CARGO_CRATES = f"{CARGO_HOME}/vendor"
VENDORED_SOURCES = "vendored-sources"
GIT_CACHE = "flatpak-cargo/git"
DEPENDENCY_TABLES = {"dependencies", "dev-dependencies", "build-dependencies"}


def canonical_url(source: str) -> str:
    """The repository URL Cargo identifies a git source by."""
    parts = urlparse(source.removeprefix("git+"))
    return f"{parts.scheme}://{parts.netloc}{parts.path.rstrip('/')}"


def checkout_name(url: str, commit: str) -> str:
    return f"{url.rsplit('/', 1)[-1]}-{commit[:7]}"


def git(*args: str, cwd: pathlib.Path) -> str:
    return subprocess.run(
        ["git", *args], cwd=cwd, check=True, capture_output=True, text=True
    ).stdout


def repository_manifests(url: str, commit: str) -> dict[str, dict[str, Any]]:
    """Every Cargo.toml of the repository at `commit`, by directory."""
    cache = pathlib.Path(os.environ.get("XDG_CACHE_HOME", pathlib.Path.home() / ".cache"))
    clone = cache / "flectar-flatpak-git" / re.sub(r"[^A-Za-z0-9.-]", "_", url)
    if not clone.exists():
        clone.parent.mkdir(parents=True, exist_ok=True)
        git("clone", "--bare", "--quiet", url, str(clone), cwd=clone.parent)
    try:
        git("cat-file", "-e", f"{commit}^{{commit}}", cwd=clone)
    except subprocess.CalledProcessError:
        git("fetch", "--quiet", "origin", commit, cwd=clone)
    manifests = {}
    for path in git("ls-tree", "-r", "--name-only", commit, cwd=clone).splitlines():
        if path == "Cargo.toml" or path.endswith("/Cargo.toml"):
            directory = path.removesuffix("Cargo.toml").rstrip("/") or "."
            manifests[directory] = tomllib.loads(git("show", f"{commit}:{path}", cwd=clone))
    return manifests


def repository_packages(url: str, commit: str) -> dict[str, tuple[str, dict, dict | None]]:
    """Each package of the repository: its directory, manifest and the
    workspace table of the nearest enclosing workspace."""
    manifests = repository_manifests(url, commit)
    packages = {}
    for directory, manifest in manifests.items():
        workspace = None
        parts = [] if directory == "." else directory.split("/")
        for depth in range(len(parts), -1, -1):
            ancestor = "/".join(parts[:depth]) or "."
            if "workspace" in manifests.get(ancestor, {}):
                workspace = manifests[ancestor]["workspace"]
                break
        if "package" in manifest:
            packages[manifest["package"]["name"]] = (directory, manifest, workspace)
    return packages


def update_workspace_keys(package: dict[str, Any], workspace: dict[str, Any] | None) -> None:
    """Replace `workspace = true` keys with the workspace's values, exactly as
    flatpak-cargo-generator's function of the same name."""
    for key, item in list(package.items()):
        if not isinstance(item, dict):
            continue
        if key == "target":
            for target in item.values():
                update_workspace_keys(target, workspace)
            continue
        if key in ("dev-dependencies", "build-dependencies"):
            update_workspace_keys(item, (workspace or {}).get("dependencies"))
            continue
        if not workspace or key not in workspace:
            continue
        workspace_item = workspace[key]
        if "workspace" in item:
            if isinstance(workspace_item, dict):
                del item["workspace"]
                for name, value in workspace_item.items():
                    if name == "features" and "features" in item:
                        item["features"] += value
                    else:
                        item[name] = value
            elif len(item) > 1:
                del item["workspace"]
                item.update({"version": workspace_item})
            else:
                package[key] = workspace_item
        else:
            update_workspace_keys(item, workspace_item)


def toml_key(key: str) -> str:
    return key if re.fullmatch(r"[A-Za-z0-9_-]+", key) else json.dumps(key, ensure_ascii=False)


def toml_value(value: Any) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, (int, float)):
        return repr(value)
    if isinstance(value, str):
        # TOML basic strings take the same escapes JSON writes.
        return json.dumps(value, ensure_ascii=False)
    if isinstance(value, list):
        return "[" + ", ".join(toml_value(item) for item in value) + "]"
    if isinstance(value, dict):
        if not value:
            return "{}"
        return "{ " + ", ".join(f"{toml_key(k)} = {toml_value(v)}" for k, v in value.items()) + " }"
    raise TypeError(f"no TOML form for {value!r}")


def is_table(path: list[str]) -> bool:
    """Whether a dictionary at `path` is written as a [table] rather than
    inline: dependency specifications and deeper values are inline."""
    if len(path) >= 2 and path[-2] in DEPENDENCY_TABLES:
        return False
    if path[0] == "target":
        return len(path) <= 3
    return len(path) <= 2


def toml_document(document: dict[str, Any]) -> str:
    lines: list[str] = []

    def table(path: list[str], values: dict[str, Any]) -> None:
        inline = [(k, v) for k, v in values.items() if not (isinstance(v, dict) and is_table([*path, k]))]
        nested = [(k, v) for k, v in values.items() if isinstance(v, dict) and is_table([*path, k])]
        if path and (inline or not nested):
            if lines:
                lines.append("")
            lines.append("[" + ".".join(toml_key(part) for part in path) + "]")
        lines.extend(f"{toml_key(k)} = {toml_value(v)}" for k, v in inline)
        for key, value in nested:
            table([*path, key], value)

    table([], document)
    text = "\n".join(lines) + "\n"
    if tomllib.loads(text) != document:
        raise SystemExit("a vendored Cargo.toml would not read back as written")
    return text


def generate() -> list[dict[str, Any]]:
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text())
    checkouts: dict[tuple[str, str], dict[str, tuple[str, dict, dict | None]]] = {}
    package_sources: list[dict[str, Any]] = []
    vendored: dict[str, dict[str, str]] = {VENDORED_SOURCES: {"directory": CARGO_CRATES}}

    for package in lock["package"]:
        source = package.get("source")
        if source is None:
            continue
        name, version = package["name"], package["version"]
        if source.startswith("registry+"):
            checksum = package["checksum"]
            destination = f"{CARGO_CRATES}/{name}-{version}"
            package_sources += [
                {
                    "type": "archive",
                    "archive-type": "tar-gzip",
                    "url": f"{CRATES_IO}/{name}/{name}-{version}.crate",
                    "sha256": checksum,
                    "dest": destination,
                },
                {
                    "type": "inline",
                    "contents": json.dumps({"package": checksum, "files": {}}),
                    "dest": destination,
                    "dest-filename": ".cargo-checksum.json",
                },
            ]
            vendored.setdefault("crates-io", {"replace-with": VENDORED_SOURCES})
        elif source.startswith("git+"):
            url = canonical_url(source)
            commit = urlparse(source).fragment
            if not commit:
                raise SystemExit(f"{name}: the git source names no commit")
            repository = checkouts.setdefault((url, commit), repository_packages(url, commit))
            directory, manifest, workspace = repository[name]
            normalized = copy.deepcopy(manifest)
            if workspace is not None:
                update_workspace_keys(normalized, workspace)
            replacement: dict[str, str] = {"git": url, "replace-with": VENDORED_SOURCES}
            query = parse_qs(urlparse(source).query)
            for key in ("rev", "tag", "branch"):
                if key in query:
                    replacement[key] = query[key][0]
                    break
            vendored[url] = replacement
            checkout = os.path.normpath(f"{GIT_CACHE}/{checkout_name(url, commit)}/{directory}")
            destination = f"{CARGO_CRATES}/{name}"
            package_sources += [
                {
                    "type": "shell",
                    "commands": [f'cp -r --reflink=auto "{checkout}" "{destination}"'],
                },
                {
                    "type": "inline",
                    "contents": toml_document(normalized),
                    "dest": destination,
                    "dest-filename": "Cargo.toml",
                },
                {
                    "type": "inline",
                    "contents": json.dumps({"package": None, "files": {}}),
                    "dest": destination,
                    "dest-filename": ".cargo-checksum.json",
                },
            ]
        else:
            raise SystemExit(f"{name}: unsupported source {source}")

    sources: list[dict[str, Any]] = [
        {
            "type": "git",
            "url": url,
            "commit": commit,
            "dest": f"{GIT_CACHE}/{checkout_name(url, commit)}",
        }
        for url, commit in checkouts
    ]
    sources += package_sources
    sources.append(
        {
            "type": "inline",
            "contents": "\n".join(
                "[source.{}]\n{}\n".format(
                    toml_key(name),
                    "\n".join(f"{toml_key(k)} = {toml_value(v)}" for k, v in table.items()),
                )
                for name, table in vendored.items()
            ),
            "dest": CARGO_HOME,
            "dest-filename": "config",
        }
    )
    return sources


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--check", action="store_true", help="fail if the file is out of date")
    arguments = parser.parse_args()
    text = json.dumps(generate(), indent=4) + "\n"
    if arguments.check:
        if OUTPUT.read_text() != text:
            print(f"{OUTPUT.relative_to(ROOT)} is out of date; run {pathlib.Path(__file__).name}.", file=sys.stderr)
            return 1
        return 0
    OUTPUT.write_text(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
