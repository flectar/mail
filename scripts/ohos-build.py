#!/usr/bin/env python3
"""Build the existing Rust/Slint application and a HarmonyOS HAP.

All caches and generated configuration stay inside this checkout. Signing
credentials are optional, local and never printed. Requires DevEco Studio and the Rust
aarch64-unknown-linux-ohos standard library target.
"""

from __future__ import annotations

import argparse
from contextlib import contextmanager
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import zipfile

ROOT = Path(__file__).resolve().parents[1]
PROJECT = ROOT / "platform" / "ohos"
TARGET = "aarch64-unknown-linux-ohos"


@contextmanager
def signing_profile(enabled: bool):
    """Use DevEco's task-specific local signing configuration only during Hvigor."""
    if not enabled:
        yield
        return
    local = require(PROJECT / ".signing/signing-configs.json")
    shared = PROJECT / "build-profile.json5"
    original = shared.read_bytes()
    config = json.loads(original)
    signing = json.loads(local.read_text())
    if not signing.get("signingConfigs"):
        raise RuntimeError("Local signing configuration contains no identity")
    app = config["app"]
    app["signingConfigs"] = signing["signingConfigs"]
    for product in app["products"]:
        local_product = next((p for p in signing["products"] if p["name"] == product["name"]), None)
        if local_product is not None:
            product["signingConfig"] = local_product["signingConfig"]
    mode = shared.stat().st_mode & 0o777
    try:
        shared.chmod(0o600)
        shared.write_text(json.dumps(config, indent=2) + "\n")
        yield
    finally:
        shared.write_bytes(original)
        shared.chmod(mode)


def run(command: list[str], env: dict[str, str], cwd: Path = ROOT) -> None:
    print("+ " + " ".join(command), flush=True)
    subprocess.run(command, cwd=cwd, env=env, check=True)


def require(path: Path) -> Path:
    if not path.is_file():
        raise RuntimeError(f"Required tool or input missing: {path}")
    return path


def environment() -> tuple[dict[str, str], Path, Path, Path]:
    env = os.environ.copy()
    studio = Path(env.get("DEVECO_STUDIO_CONTENTS", "/Applications/DevEco-Studio.app/Contents"))
    native_sdk = Path(env.get("OHOS_SDK", str(studio / "sdk/default/openharmony/native")))
    node = require(studio / "tools/node/bin/node")
    hvigor = require(studio / "tools/hvigor/hvigor/bin/hvigor.js")
    ohpm = require(studio / "tools/ohpm/bin/pm-cli.js")
    compiler = require(native_sdk / "llvm/bin/aarch64-unknown-linux-ohos-clang")
    cache = PROJECT / ".cache"
    hvigor_home = cache / "hvigor"
    modules = hvigor_home / "node_modules"
    (modules / "@ohos").mkdir(parents=True, exist_ok=True)
    for package in ("hvigor", "hvigor-ohos-plugin"):
        destination = modules / "@ohos" / package
        if not destination.exists():
            destination.symlink_to(studio / "tools/hvigor" / package, target_is_directory=True)
    (cache / "ohpm").mkdir(parents=True, exist_ok=True)
    env.update({
        "OHOS_SDK": str(native_sdk),
        "DEVECO_SDK_HOME": env.get("DEVECO_SDK_HOME", str(studio / "sdk")),
        "JAVA_HOME": env.get("JAVA_HOME", str(studio / "jbr/Contents/Home")),
        "CARGO_TARGET_DIR": str(ROOT / "target"),
        "CARGO_INCREMENTAL": "0",
        "HVIGOR_USER_HOME": str(hvigor_home),
        "NODE_PATH": str(modules) + (os.pathsep + env["NODE_PATH"] if env.get("NODE_PATH") else ""),
        "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_OHOS_LINKER": str(compiler),
        "CC_aarch64_unknown_linux_ohos": str(compiler),
        "CXX_aarch64_unknown_linux_ohos": str(require(native_sdk / "llvm/bin/aarch64-unknown-linux-ohos-clang++")),
        "AR_aarch64_unknown_linux_ohos": str(require(native_sdk / "llvm/bin/llvm-ar")),
        "RANLIB_aarch64_unknown_linux_ohos": str(require(native_sdk / "llvm/bin/llvm-ranlib")),
        "BINDGEN_EXTRA_CLANG_ARGS_aarch64_unknown_linux_ohos": f"--target={TARGET} --sysroot={native_sdk / 'sysroot'}",
        "PATH": os.pathsep.join((str(node.parent), str(native_sdk / "build-tools/cmake/bin"), env.get("PATH", ""))),
    })
    if (native_sdk / "llvm/lib/libclang.dylib").is_file():
        env["LIBCLANG_PATH"] = str(native_sdk / "llvm/lib")
    return env, node, hvigor, ohpm


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--rust-only", action="store_true", help="Compile only the native Rust archive")
    mode.add_argument("--hap-only", action="store_true", help="Package an already compiled Rust archive")
    mode.add_argument("--arkts-only", action="store_true", help="Type-check and compile the Stage shell")
    parser.add_argument("--debug", action="store_true", help="Build Rust and HAP in debug mode")
    parser.add_argument("--sign", action="store_true", help="Use this app's ignored local DevEco signing identity")
    args = parser.parse_args()
    env, node, hvigor, ohpm = environment()
    build_mode = "debug" if args.debug else "release"
    profile = "debug" if args.debug else "release"
    archive = ROOT / "target" / TARGET / profile / "libfeihong_rust.a"
    env["FEIHONG_RUST_LIBRARY"] = str(archive)

    if not args.hap_only and not args.arkts_only:
        rustup = shutil.which("rustup")
        cargo = shutil.which("cargo")
        if not rustup or not cargo:
            raise RuntimeError("Rustup and Cargo must be installed before building")
        targets = subprocess.check_output([rustup, "target", "list", "--installed"], env=env, text=True).splitlines()
        if TARGET not in targets:
            raise RuntimeError(f"Install the Rust target first: rustup target add {TARGET}")
        # Cargo treats a watched missing input as dirty on every invocation.
        # Preserve upstream OAuth .env loading, including newly created files,
        # by choosing it before Cargo runs and otherwise watching a stable empty
        # build input. Runtime custom OAuth configuration remains independent.
        if "FLECTAR_OAUTH_BUILD_ENV_FILE" not in env:
            oauth_env = ROOT / ".env"
            if not oauth_env.is_file():
                oauth_env = ROOT / "target/ohos/oauth-build.env"
                oauth_env.parent.mkdir(parents=True, exist_ok=True)
                if not oauth_env.exists():
                    descriptor = os.open(oauth_env, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
                    os.close(descriptor)
            env["FLECTAR_OAUTH_BUILD_ENV_FILE"] = str(oauth_env)
        openssl_builder = ROOT / "scripts/ohos-openssl.py"
        if openssl_builder.exists():
            run([sys.executable, str(openssl_builder), "--sdk", env["OHOS_SDK"]], env)
        # Generated OHOS OpenSSL is discovered by openssl-sys without using the host SDK.
        openssl_prefix = ROOT / "target" / "ohos" / "openssl" / "arm64-v8a"
        if openssl_prefix.exists():
            env["OPENSSL_DIR"] = str(openssl_prefix)
            env["OPENSSL_LIB_DIR"] = str(openssl_prefix / "lib")
            env["OPENSSL_INCLUDE_DIR"] = str(openssl_prefix / "include")
            env["OPENSSL_LIBS"] = "ssl:crypto"
            env["OPENSSL_STATIC"] = "1"
        command = [cargo, "build", "--locked", "--manifest-path", str(PROJECT / "rust/Cargo.toml"), "--target", TARGET]
        if not args.debug:
            command.append("--release")
        run(command, env)
        require(archive)
    if args.rust_only:
        print(f"Rust archive: {archive}")
        return 0

    if not args.arkts_only:
        require(archive)
        require(PROJECT / "entry/libs/arm64-v8a/libpdfium.so")
        require(PROJECT / "entry/src/main/resources/rawfile/pdfium-licenses/LICENSE")
        run([sys.executable, str(require(ROOT / "scripts/ohos-licenses.py"))], env)

    run([str(node), str(ohpm), "install", "--all", "--cache", str(PROJECT / ".cache/ohpm"),
         "--no-experimental-concurrently-safe"], env, PROJECT)
    task = "default@CompileArkTS" if args.arkts_only else "assembleHap"
    sign = args.sign and not args.arkts_only
    with signing_profile(sign):
        run([str(node), str(hvigor), "--mode", "module", "-p", "product=default", "-p", "module=entry@default",
             "-p", f"buildMode={build_mode}", "-p", "enableSignTask=" + str(sign).lower(),
             task, "--no-daemon", "--no-parallel"], env, PROJECT)
    if args.arkts_only:
        return 0
    suffix = "signed" if args.sign else "unsigned"
    hap = require(PROJECT / f"entry/build/default/outputs/default/entry-default-{suffix}.hap")
    with zipfile.ZipFile(hap) as package:
        names = package.namelist()
        for name in ("feihong", "pdfium"):
            path = f"libs/arm64-v8a/lib{name}.so"
            if path not in names:
                raise RuntimeError(f"HAP is missing the native {name} library")
            with package.open(path) as library:
                header = library.read(20)
            if len(header) != 20 or header[:5] != b"\x7fELF\x02" or header[5] != 1 or int.from_bytes(header[18:20], "little") != 183:
                raise RuntimeError(f"HAP contains an invalid AArch64 ELF library: {name}")
        if "resources/rawfile/pdfium-licenses/LICENSE" not in names:
            raise RuntimeError("HAP is missing the bundled PDFium license")
    print(f"{'Signed' if args.sign else 'Unsigned'} HAP: {hap}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (RuntimeError, subprocess.CalledProcessError) as error:
        print(f"HarmonyOS build failed: {error}", file=sys.stderr)
        raise SystemExit(1)
