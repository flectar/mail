#!/usr/bin/env python3
"""Build the Microsoft Store packages from the staged Windows release files.

`pack` turns one architecture's portable layout (target/package/windows) into
an unsigned .msix with the manifest and logos from platform/windows/msix.
`bundle` combines the architectures' packages into one .msixbundle, the file
uploaded to Partner Center. The Store signs both, so neither is signed here.

The tools come from the newest installed Windows SDK. The MAKEAPPX and
MAKEPRI environment variables can name them explicitly.
"""

import argparse
import os
import platform
import re
import shutil
import subprocess
import tempfile
import xml.etree.ElementTree as ET
from pathlib import Path

PROJECT = Path(__file__).resolve().parent.parent
MSIX = PROJECT / "platform/windows/msix"
ARCHITECTURES = ("x64", "arm64")
MANIFEST = "{http://schemas.microsoft.com/appx/manifest/foundation/windows10}"
# The manifest ships with this publisher until the Partner Center identity
# replaces it; Partner Center rejects packages that still carry it.
PLACEHOLDER_PUBLISHER = "CN=00000000-0000-0000-0000-000000000000"


def sdk_tool(name):
    """The tool from the newest Windows SDK, preferring native Arm64 builds on Arm64."""
    override = os.environ.get(Path(name).stem.upper())
    if override:
        return override
    kits = Path(os.environ.get("ProgramFiles(x86)", r"C:\Program Files (x86)")) / "Windows Kits/10/bin"
    hosts = ("arm64", "x64", "x86") if platform.machine().upper() == "ARM64" else ("x64", "x86")
    versions = [path for path in kits.glob("10.*") if re.fullmatch(r"[0-9.]+", path.name)]
    for version in sorted(versions, key=lambda path: tuple(map(int, path.name.split("."))), reverse=True):
        for host in hosts:
            if (tool := version / host / name).is_file():
                return str(tool)
    raise FileNotFoundError(f"{name} was not found in a Windows SDK under {kits}")


def run(*command):
    print("+", " ".join(map(str, command)), flush=True)
    subprocess.run([str(part) for part in command], check=True)


def check_version(version):
    if not re.fullmatch(r"[1-9][0-9]{0,4}\.[0-9]{1,5}\.[0-9]{1,5}\.0", version) or any(
            int(part) > 65535 for part in version.split(".")):
        raise ValueError(f"Store versions are Major.Minor.Build.0 with parts up to 65535, not {version}")


def manifest(version, arch):
    check_version(version)
    if arch not in ARCHITECTURES:
        raise ValueError(f"Unsupported Store package architecture: {arch}")
    text = (MSIX / "AppxManifest.xml.in").read_text(encoding="utf-8")
    for placeholder, value in (("@VERSION@", version), ("@ARCHITECTURE@", arch)):
        if text.count(placeholder) != 1:
            raise ValueError(f"The manifest template must contain {placeholder} once")
        text = text.replace(placeholder, value)
    return text


def warn_placeholder_identity(text):
    publisher = ET.fromstring(text).find(f"{MANIFEST}Identity").get("Publisher")
    if publisher == PLACEHOLDER_PUBLISHER:
        message = ("The Store package still has the placeholder identity. "
                   "See docs/microsoft-store.md before uploading it to Partner Center.")
        prefix = "::warning title=Placeholder Store identity::" if os.environ.get("GITHUB_ACTIONS") else "warning: "
        print(prefix + message, flush=True)


def pri_config(makepri, directory):
    """makepri's default configuration without automatic resource packages,
    which only apply to bundles that split out their resources."""
    config = directory / "priconfig.xml"
    run(makepri, "createconfig", "/cf", config, "/dq", "en-US", "/pv", "10.0.0", "/o")
    tree = ET.parse(config)
    for packaging in tree.getroot().findall("packaging"):
        tree.getroot().remove(packaging)
    tree.write(config, encoding="utf-8", xml_declaration=True)
    return config


def pack(arch, version, layout_source, output):
    for name in ("flectar-mail.exe", "pdfium.dll"):
        if not (layout_source / name).is_file():
            raise FileNotFoundError(f"{layout_source / name} is missing; stage the Windows package first")
    text = manifest(version, arch)
    makeappx, makepri = sdk_tool("makeappx.exe"), sdk_tool("makepri.exe")
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="flectar-msix-") as directory:
        directory = Path(directory)
        layout = directory / "layout"
        shutil.copytree(layout_source, layout)
        (layout / "AppxManifest.xml").write_text(text, encoding="utf-8")
        # Index only the logos: Windows resolves their scale-, targetsize- and
        # altform- variants through resources.pri.
        indexed = directory / "pri"
        shutil.copytree(MSIX / "Assets", indexed / "Assets")
        shutil.copytree(MSIX / "Assets", layout / "Assets")
        run(makepri, "new", "/pr", indexed, "/cf", pri_config(makepri, directory),
            "/mn", layout / "AppxManifest.xml", "/of", layout / "resources.pri", "/o")
        run(makeappx, "pack", "/o", "/h", "SHA256", "/d", layout, "/p", output)
    warn_placeholder_identity(text)
    print(f"Built {output}")


def bundle(version, packages, output):
    check_version(version)
    if len({package.name for package in packages}) != len(packages):
        raise ValueError("Bundled packages need distinct file names")
    makeappx = sdk_tool("makeappx.exe")
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="flectar-msixbundle-") as directory:
        for package in packages:
            shutil.copyfile(package, Path(directory) / package.name)
        run(makeappx, "bundle", "/o", "/bv", version, "/d", directory, "/p", output)
    print(f"Built {output}")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    commands = parser.add_subparsers(dest="command", required=True)
    single = commands.add_parser("pack", help="package one architecture")
    single.add_argument("--arch", choices=ARCHITECTURES, required=True)
    single.add_argument("--version", required=True, help="Store version, from release-metadata.py")
    single.add_argument("layout", type=Path, help="staged Windows release files")
    single.add_argument("output", type=Path, help=".msix to write")
    combined = commands.add_parser("bundle", help="bundle the architectures' packages")
    combined.add_argument("--version", required=True, help="Store version, from release-metadata.py")
    combined.add_argument("output", type=Path, help=".msixbundle to write")
    combined.add_argument("packages", type=Path, nargs="+", help=".msix packages")
    args = parser.parse_args()
    try:
        if args.command == "pack":
            pack(args.arch, args.version, args.layout, args.output)
        else:
            bundle(args.version, args.packages, args.output)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        raise SystemExit(f"error: {error}") from error


if __name__ == "__main__":
    main()
