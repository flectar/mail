#!/usr/bin/env python3
"""Build pinned OpenSSL static libraries for the Sequoia OHOS engine.

Only source/dependency outputs are downloaded or written. The final Rust
library statically links libssl and libcrypto; no executable, shared provider,
or gpg is shipped. The openssl crate's initialization and generated FFI wrappers
also reference SSL entry points, even when OpenPGP only uses crypto operations.
Pass the printed OPENSSL_LIBS=ssl:crypto environment variable to Cargo.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tarfile
import urllib.request

VERSION = "3.5.9"
SOURCE = f"https://github.com/openssl/openssl/releases/download/openssl-{VERSION}/openssl-{VERSION}.tar.gz"
# Published by OpenSSL alongside the official LTS release.
SHA256 = "603f5602e2eef00d77fbd429d34dcd5822bb301757a1bc9cdb24c670f1eb859a"
ROOT = Path(__file__).resolve().parents[1]
ABI = {
    "arm64-v8a": ("aarch64-linux-ohos", "linux-aarch64"),
    "x86_64": ("x86_64-unknown-linux-ohos", "linux-x86_64"),
}


def run(args, *, cwd, env):
    print("+ " + shlex.join(map(str, args)), flush=True)
    subprocess.run(args, cwd=cwd, env=env, check=True)


def extract(archive, destination):
    # Reject path/link escapes even on Python versions without tar's data filter.
    with tarfile.open(archive, "r:gz") as tar:
        for member in tar.getmembers():
            target = destination / member.name
            if not target.resolve().is_relative_to(destination.resolve()):
                raise RuntimeError("OpenSSL source archive contains an unsafe path")
            if member.issym() or member.islnk():
                linked = (target.parent if member.issym() else destination) / member.linkname
                if not linked.resolve().is_relative_to(destination.resolve()):
                    raise RuntimeError("OpenSSL source archive contains an unsafe link")
            if member.isdev():
                raise RuntimeError("OpenSSL source archive contains a device")
        tar.extractall(destination, filter="data")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sdk", type=Path, default=Path(os.environ.get("OHOS_NDK_HOME", "/Applications/DevEco-Studio.app/Contents/sdk/default/openharmony/native")))
    parser.add_argument("--abi", choices=ABI, default="arm64-v8a")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--source-archive", type=Path)
    parser.add_argument("--jobs", type=int, default=min(os.cpu_count() or 2, 8))
    parser.add_argument("--api", type=int, default=20)
    args = parser.parse_args()
    sdk = args.sdk.resolve()
    output = (args.output or ROOT / "target" / "ohos" / "openssl" / args.abi).resolve()
    triple, platform = ABI[args.abi]
    llvm = sdk / "llvm" / "bin"
    for path in (llvm / "clang", llvm / "llvm-ar", llvm / "llvm-ranlib", sdk / "sysroot"):
        if not path.exists():
            parser.error(f"Missing OHOS SDK input: {path}")
    if args.jobs < 1 or args.api < 20:
        parser.error("jobs must be positive; this port requires API >= 20")
    output.mkdir(parents=True, exist_ok=True)
    cargo = {"OPENSSL_DIR": str(output), "OPENSSL_LIB_DIR": str(output / "lib"), "OPENSSL_INCLUDE_DIR": str(output / "include"), "OPENSSL_STATIC": "1", "OPENSSL_LIBS": "ssl:crypto"}
    manifest_path = output / "build-manifest.json"
    reusable = False
    if manifest_path.exists() and (output / "lib" / "libcrypto.a").exists() and (output / "include" / "openssl" / "configuration.h").exists():
        manifest = json.loads(manifest_path.read_text())
        reusable = all(manifest.get(key) == value for key, value in {"version": VERSION, "sha256": SHA256, "abi": args.abi, "target": triple, "api": args.api, "sdk": str(sdk)}.items())
        if reusable and (output / "lib" / "libssl.a").exists():
            print(json.dumps({"artifacts": [str(output / "lib" / name) for name in ("libssl.a", "libcrypto.a")], "cargo_env": cargo, "cached": True}, indent=2), flush=True)
            return
    archive = args.source_archive.resolve() if args.source_archive else output / f"openssl-{VERSION}.tar.gz"
    if not archive.exists():
        print(f"Downloading {SOURCE}", flush=True)
        download = archive.with_name(archive.name + ".download")
        try:
            with urllib.request.urlopen(SOURCE, timeout=120) as response, download.open("wb") as sink:
                shutil.copyfileobj(response, sink)
            with download.open("rb") as source_bytes:
                if hashlib.file_digest(source_bytes, "sha256").hexdigest() != SHA256:
                    raise RuntimeError("Downloaded OpenSSL source checksum mismatch")
            download.replace(archive)
        finally:
            download.unlink(missing_ok=True)
    digest = hashlib.file_digest(archive.open("rb"), "sha256").hexdigest()
    if digest != SHA256:
        raise RuntimeError(f"OpenSSL source checksum mismatch: {digest}")
    sources = output / "source"
    sources.mkdir(exist_ok=True)
    source = sources / f"openssl-{VERSION}"
    if not source.exists():
        extract(archive, sources)
    env = os.environ.copy()
    for variable in ("CPPFLAGS", "LDFLAGS", "CPATH", "C_INCLUDE_PATH", "CPLUS_INCLUDE_PATH", "LIBRARY_PATH"):
        env.pop(variable, None)
    env.update({
        "CC": shlex.join([str(llvm / "clang"), f"--target={triple}", f"--sysroot={sdk / 'sysroot'}"]),
        "AR": str(llvm / "llvm-ar"),
        "RANLIB": str(llvm / "llvm-ranlib"),
        "CFLAGS": f"-Os -fPIC -fvisibility=hidden -D__OHOS__ -D__OHOS_API__={args.api}",
        "SOURCE_DATE_EPOCH": "1790640000",
    })
    configure = ["perl", "Configure", platform, "no-shared", "no-tests", "no-module", "no-dso", "no-engine", "no-async", "no-afalgeng", f"--prefix={output}", "--libdir=lib"]
    # Configuring the same source tree for another ABI is forbidden: each ABI
    # has its own output/source directory, including generated headers.
    if not (reusable and (source / "configdata.pm").is_file() and (source / "Makefile").is_file()):
        run(configure, cwd=source, env=env)
    run(["make", f"-j{args.jobs}", "build_generated", "libssl.a", "libcrypto.a"], cwd=source, env=env)
    (output / "lib").mkdir(exist_ok=True)
    for library in ("libssl.a", "libcrypto.a"):
        shutil.copy2(source / library, output / "lib" / library)
    shutil.copytree(source / "include" / "openssl", output / "include" / "openssl", dirs_exist_ok=True)
    shutil.copy2(source / "LICENSE.txt", output / "OpenSSL-LICENSE.txt")
    with (output / "configdata.txt").open("w") as config:
        subprocess.run(["perl", "configdata.pm", "--dump"], cwd=source, env=env, check=True, stdout=config)
    manifest = {"name": "OpenSSL", "version": VERSION, "license": "Apache-2.0", "source": SOURCE, "sha256": SHA256,
                "abi": args.abi, "target": triple, "api": args.api, "configure": configure,
                "sdk": str(sdk), "libraries": ["libssl.a", "libcrypto.a"], "compiler": subprocess.check_output([str(llvm / "clang"), "--version"], text=True).splitlines()[0]}
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps({"artifacts": [str(output / "lib" / name) for name in ("libssl.a", "libcrypto.a")], "cargo_env": cargo}, indent=2), flush=True)


if __name__ == "__main__":
    main()
