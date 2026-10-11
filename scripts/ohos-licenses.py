#!/usr/bin/env python3
"""Stage actual OHOS CPU runtime dependency notices without network access.

Build-only dependencies and proc-macro subtrees are excluded. Native PDFium
notices are staged by its own builder in rawfile/pdfium-licenses.
"""
import hashlib
import json
import pathlib
import re
import shutil
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[1]
OUTPUT = ROOT / "platform/ohos/entry/src/main/resources/rawfile/licenses"
TARGET = "aarch64-unknown-linux-ohos"


def copy(source, destination):
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, destination)


def runtime_packages():
    result = subprocess.run([
        "cargo", "metadata", "--manifest-path", str(ROOT / "platform/ohos/rust/Cargo.toml"),
        "--format-version", "1", "--filter-platform", TARGET, "--locked", "--offline",
    ], cwd=ROOT, check=True, capture_output=True, text=True)
    metadata = json.loads(result.stdout)
    packages = {package["id"]: package for package in metadata["packages"]}
    # metadata's resolve graph unifies host build features with target runtime
    # features. cargo tree uses the target's actual unit graph and excludes the
    # Slint compiler's host renderers and all proc-macro subtrees here.
    tree = subprocess.run([
        "cargo", "tree", "--manifest-path", str(ROOT / "platform/ohos/rust/Cargo.toml"),
        "--target", TARGET, "--edges", "normal,no-proc-macro", "--prefix", "none",
        "--format", "{p}", "--locked", "--offline",
    ], cwd=ROOT, check=True, capture_output=True, text=True)
    active = set()
    for line in tree.stdout.splitlines():
        match = re.match(r"^(\S+) v(\S+)", line)
        if match:
            active.add(match.groups())
    selected = [package for package in packages.values()
                if (package["name"], package["version"]) in active]
    if len(selected) != len(active):
        raise RuntimeError("runtime package identities are ambiguous or absent in cargo metadata")
    return sorted(selected, key=lambda package: (package["name"], package["version"]))


def license_files(package):
    directory = pathlib.Path(package["manifest_path"]).parent
    files = set()
    if package.get("license_file"):
        files.add(directory / package["license_file"])
    for file in directory.iterdir():
        if file.is_file() and file.name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "COPYRIGHT", "NOTICE")):
            files.add(file)
        elif file.is_dir() and file.name.lower() in ("licenses", "licences"):
            files.update(child for child in file.rglob("*") if child.is_file())
    return directory, sorted(file for file in files if file.is_file())


def main():
    packages = runtime_packages()
    forbidden = {"wgpu", "wgpu-hal", "skia-safe", "i-slint-renderer-skia", "winit", "aws-lc-sys", "keyring"}
    unexpected = forbidden.intersection(package["name"] for package in packages)
    if unexpected:
        raise RuntimeError(f"CPU runtime unexpectedly contains {sorted(unexpected)}")
    OUTPUT.mkdir(parents=True, exist_ok=True)
    # This directory is entirely generated; remove old graph notices so a new
    # build cannot retain components that are no longer in the runtime.
    shutil.rmtree(OUTPUT / "rust", ignore_errors=True)
    # Native/source notices have stable names for the shipped index.
    copy(ROOT / "LICENSE", OUTPUT / "AGPL-3.0-only.txt")
    copy(ROOT / "TRADEMARKS.md", OUTPUT / "FLECTAR-TRADEMARKS.md")
    copy(ROOT / "target/ohos/openssl/arm64-v8a/OpenSSL-LICENSE.txt", OUTPUT / "OpenSSL-Apache-2.0.txt")
    copy(ROOT / "crates/jmap-client/OHOS-PATCHES.md", OUTPUT / "JMAP-OHOS-PATCHES.md")
    copy(ROOT / "crates/wgpu-hal-ohos29/OHOS-PATCHES.md", OUTPUT / "OPTIONAL-GPU-SOURCE.md")
    copy(ROOT / "ui/icons/lucide/LICENSE", OUTPUT / "Lucide-ISC.txt")
    for font in ("google-sans-flex", "noto-emoji"):
        copy(ROOT / f"resources/fonts/{font}/OFL.txt", OUTPUT / f"fonts/{font}/OFL.txt")
        copy(ROOT / f"resources/fonts/{font}/README.md", OUTPUT / f"fonts/{font}/README.md")

    entries = []
    for package in packages:
        directory, files = license_files(package)
        relative = pathlib.Path("rust") / f"{package['name']}-{package['version']}"
        paths = []
        for file in files:
            path = relative / file.relative_to(directory)
            copy(file, OUTPUT / path)
            paths.append(path.as_posix())
        if not paths:
            path = relative / "PACKAGE-NOTICE.json"
            declaration = {key: package.get(key) for key in ("name", "version", "authors", "license", "repository", "homepage")}
            declaration["notice"] = "The published Cargo package contains no standalone license file. These are its original package license and author declarations."
            (OUTPUT / path).parent.mkdir(parents=True, exist_ok=True)
            (OUTPUT / path).write_text(json.dumps(declaration, ensure_ascii=False, indent=2) + "\n")
            paths.append(path.as_posix())
        entries.append({"name": package["name"], "version": package["version"],
                        "license": package["license"], "repository": package["repository"],
                        "notices": paths})
    sequoia = next(entry for entry in entries if entry["name"] == "sequoia-openpgp")
    copy(OUTPUT / sequoia["notices"][0], OUTPUT / "Sequoia-LGPL-2.0-or-later.txt")
    lock = ROOT / "platform/ohos/rust/Cargo.lock"
    manifest = {"target": TARGET, "renderer": "CPU", "cargo_lock_sha256": hashlib.sha256(lock.read_bytes()).hexdigest(),
                "upstream": {"repository": "https://github.com/flectar/mail", "revision": "bc0f966846a6d1758bb39ea54a24be4de6cad8d7", "license": "AGPL-3.0-only"},
                "native": [{"name": "OpenSSL", "version": "3.5.9", "license": "Apache-2.0",
                            "source": "https://github.com/openssl/openssl/releases/tag/openssl-3.5.9", "notices": ["OpenSSL-Apache-2.0.txt"]}],
                "rust_runtime_dependencies": entries}
    pdfium_notices = OUTPUT.parent / "pdfium-licenses"
    pdfium_library = ROOT / "platform/ohos/entry/libs/arm64-v8a/libpdfium.so"
    pdfium = None
    if pdfium_library.is_file() and (pdfium_notices / "build.json").is_file():
        pdfium = json.loads((pdfium_notices / "build.json").read_text())
        if hashlib.sha256(pdfium_library.read_bytes()).hexdigest() != pdfium["sha256"]:
            raise RuntimeError("staged PDFium library does not match its license/build manifest")
        manifest["native"].append({"name": "PDFium", "version": pdfium["release"],
                                   "revision": pdfium["revision"], "license": "BSD-3-Clause and bundled component licenses",
                                   "source": pdfium["source"], "notices": ["../pdfium-licenses/LICENSE", "../pdfium-licenses/third_party/", "../pdfium-licenses/build.json"]})
    (OUTPUT / "runtime-dependencies.json").write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + "\n")
    lines = ["# 飞鸿邮箱 · 源码与第三方许可", "", "飞鸿邮箱基于 Flectar Mail，基线 bc0f966846a6d1758bb39ea54a24be4de6cad8d7。此移植并非 Flectar 出品、赞助或支持。", "",
             "应用与完整改动源码遵循 AGPL-3.0-only；原上游版权、商标政策和各组件许可仍保留。重建与重链步骤见随源码提供的 platform/ohos/README.md、scripts/ohos-build.py 和 scripts/ohos-openssl.py。修改版源码应与发布包一起交付。", "",
             "- AGPL 全文：AGPL-3.0-only.txt；上游来源：https://github.com/flectar/mail", "- Sequoia OpenPGP 2.4.1：Sequoia-LGPL-2.0-or-later.txt（保留原始许可声明和 GNU Library GPL v2 全文）", "- OpenSSL 3.5.9 静态 libssl + libcrypto：OpenSSL-Apache-2.0.txt", "- JMAP 客户端 0.4.2：rust/jmap-client-0.4.2/LICENSE-APACHE、LICENSE-MIT；本地修改见 JMAP-OHOS-PATCHES.md", "- Slint 1.18.1：本 AGPL 开源构建使用 GPL-3.0-only 许可选项，全文见 rust/slint-1.18.1/LICENSES/GPL-3.0-only.txt", "- Lucide 图标：Lucide-ISC.txt；内嵌 Google Sans Flex 与 Noto Emoji 字体：fonts/ 下 OFL 全文和来源", "",
             "当前为 CPU 构建。OPTIONAL-GPU-SOURCE.md 只说明仓库中未启用的 WGPU29 OHOS 源码补丁，不将它列为当前 HAP 的运行依赖。", "",
             (f"PDFium 原生库已打包，版本 {pdfium['release']}，源码版本 {pdfium['revision']}；独立 notices 位于 rawfile/pdfium-licenses/LICENSE、third_party/ 和 build.json。" if pdfium else "PDFium 原生库尚未打包；打包时应同步附带其独立许可目录 rawfile/pdfium-licenses。"), "",
             "以下列表来自本次 OHOS CPU Cargo 运行依赖解析，排除了 build-only 与 proc-macro 子树；固定版本与许可文件路径同时记录在 runtime-dependencies.json。", "", "| 组件 | 版本 | 许可声明 |", "| --- | --- | --- |"]
    lines.extend(f"| {entry['name']} | {entry['version']} | {entry['license'] or 'See component source'} |" for entry in entries)
    (OUTPUT / "README.md").write_text("\n".join(lines) + "\n")
    print(json.dumps({"output": str(OUTPUT), "runtime_packages": len(entries), "license_files": sum(len(entry["notices"]) for entry in entries)}))


if __name__ == "__main__":
    main()
