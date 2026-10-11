#!/usr/bin/env python3
"""Build PDFium 7881 from pinned source with the HarmonyOS NDK, never Android ABI.

GN uses its Linux/POSIX source selection. C++ requires Clang 21 or newer and
PDFium's pinned private libc++; all code targets the OHOS sysroot and the NDK
links the shared library. V8/XFA are disabled. No SDK PDF component replaces
PDFium.
"""
import argparse
import hashlib
import json
import os
import pathlib
import platform
import re
import shutil
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[1]
PDFIUM_REVISION = "91b9d569b34be4f38eed7b3c49b227356c3aadad"
DEPOT_REVISION = "a03dfbf7f2d1cefa2795d81d59ca4f16c8616a8f"
GN_REVISION = "3357c4f51b1a9e676378c695dd9c7e9911c35ee6"
SIMDUTF_REVISION = "f7356eed293f8208c40b3c1b344a50bd70971983"
LIBCXX_REVISION = "5abc7f839700f0f17338434e1c1c6a8c87c00c11"
LIBCXXABI_REVISION = "8f11bb1d4438d0239d0dfc1bd9456a9f31629dda"
BUILD_REVISION = "613f5c13bccbc15bd7ce8da9acb13ac06459f8cb"
BUILDTOOLS_REVISION = "e06be5db47ae15871b2c40c44d75a9fb91daf194"
PATCHED_PATHS = ["BUILD.gn", "public/fpdfview.h", "core/fxge/linux/fx_linux_impl.cpp",
                 "build/config/c++/BUILD.gn", "buildtools/third_party/libc++/BUILD.gn"]
SKIP_DEPS = [
    "third_party/llvm-build/Release+Asserts", "third_party/rust-toolchain",
    "third_party/clang-format/script", "buildtools/reclient", "third_party/ninja",
    "tools/clang", "tools/rust", "tools/code_coverage", "tools/memory", "tools/win",
    "third_party/siso/cipd",
    "third_party/libunwind/src",
]
EXPLICIT_LICENSE_FILES = [
    # The generic LICENSE files reference these actual licensing terms.
    "third_party/freetype/src/docs/FTL.TXT",
    "third_party/libjpeg_turbo/README.ijg",
]


def run(args, cwd=None, capture=False):
    print("+", " ".join(map(str, args)), flush=True)
    return subprocess.run(list(map(str, args)), cwd=cwd, check=True,
                          text=True, stdout=subprocess.PIPE if capture else None,
                          env={**os.environ, "DEPOT_TOOLS_UPDATE": "0", "DEPOT_TOOLS_METRICS": "0"}).stdout


def checkout(destination, url, revision):
    if not (destination / ".git").is_dir():
        destination.mkdir(parents=True, exist_ok=True)
        run(["git", "init", destination])
        run(["git", "remote", "add", "origin", url], destination)
    result = subprocess.run(["git", "rev-parse", "HEAD"], cwd=destination, text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
    head = result.stdout.strip() if result.returncode == 0 else ""
    if head != revision:
        run(["git", "fetch", "--depth", "1", "origin", revision], destination)
        run(["git", "checkout", "--detach", revision], destination)


def replace_once(path, old, new):
    text = path.read_text()
    if new in text:
        return
    if text.count(old) != 1:
        raise ValueError(f"Pinned source patch no longer matches: {path}")
    path.write_text(text.replace(old, new, 1))


def configure(source, sdk, cxx):
    if not (source / "build/config/gclient_args.gni").is_file():
        (source / "build/config/gclient_args.gni").write_text('''build_with_chromium = false
checkout_android = false
checkout_skia = false
android_ndk_version = "2@30.0.14608247"
''')
    # Build one shared library and export the public FPDF C ABI despite the
    # non-component build. This is the same distinction PDFium binaries use.
    replace_once(source / "BUILD.gn", 'component("pdfium") {', 'shared_library("pdfium") {')
    header = source / "public/fpdfview.h"
    text = header.read_text()
    if '#if defined(COMPONENT_BUILD)' in text:
        start = text.index('#if defined(COMPONENT_BUILD)')
        end = text.index('#endif  // defined(COMPONENT_BUILD)', start)
        block = text[start:end + len('#endif  // defined(COMPONENT_BUILD)')]
        block = block.replace('#if defined(COMPONENT_BUILD)\n', '', 1)
        block = block.removesuffix('#else\n#define FPDF_EXPORT\n#endif  // defined(COMPONENT_BUILD)')
        header.write_text(text[:start] + block + text[end + len('#endif  // defined(COMPONENT_BUILD)'):])
    replace_once(source / "core/fxge/linux/fx_linux_impl.cpp",
                 'pInfo->AddPath("/usr/share/fonts");',
                 'pInfo->AddPath("/system/fonts");\n      pInfo->AddPath("/usr/share/fonts");')
    # is_clang=false avoids Chromium-23 compiler flags and plugins unsupported
    # by the NDK C compiler. Correct the two GCC-only runtime links for our
    # Clang toolchain; the OHOS driver supplies compiler-rt and libunwind.
    replace_once(source / "build/config/c++/BUILD.gn", '''      ldflags += [ "-nodefaultlibs" ]

      # Unfortunately, there's no way to disable linking against just libc++
      # (gcc doesn't have -notstdlib++:
      # https://gcc.gnu.org/bugzilla/show_bug.cgi?id=83931); -nodefaultlibs
      # removes all of the default libraries, so add back the ones that we need.
      libs += [
        "c",
        "gcc_s",
        "m",
        "rt",
      ]''', '''      # Feihong's OHOS NDK driver supplies compiler-rt/unwind/libc.
      ldflags += [ "-nostdlib++" ]''')
    replace_once(source / "buildtools/third_party/libc++/BUILD.gn",
                 '  if (is_linux && !is_clang) {\n    libs = [ "atomic" ]\n  }',
                 '  if (is_linux && !is_clang && current_toolchain != "//build/toolchain/feihong:ohos") {\n    libs = [ "atomic" ]\n  }')
    (source / ".feihong-patches.json").write_text(json.dumps({
        name: hashlib.sha256((source / name).read_bytes()).hexdigest() for name in PATCHED_PATHS
    }))
    toolchain = source / "build/toolchain/feihong"
    toolchain.mkdir(parents=True, exist_ok=True)
    llvm, sysroot = sdk / "llvm/bin", sdk / "sysroot"
    flags = f'--target=aarch64-linux-ohos --sysroot="{sysroot}" -Wno-unknown-warning-option'
    # These are also Ninja input paths; embedding shell quotes makes them literal
    # filenames. Keep the SDK path plain when passed to the GNU template.
    def gn(value):
        return json.dumps(value)
    (toolchain / "BUILD.gn").write_text(f'''import("//build/toolchain/gcc_toolchain.gni")
gcc_toolchain("ohos") {{
  cc = {gn(str(llvm / 'clang'))}
  cxx = {gn(str(cxx))}
  ar = {gn(str(llvm / 'llvm-ar'))}
  nm = {gn(str(llvm / 'llvm-nm'))}
  readelf = {gn(str(llvm / 'llvm-readelf'))}
  ld = {gn(str(llvm / 'clang++'))}
  extra_cppflags = {gn(flags)}
  # This is the existing Chromium libc++ switch for its musl implementation.
  extra_cxxflags = "-DANDROID_HOST_MUSL=1"
  extra_asmflags = {gn(flags)}
  extra_ldflags = {gn(flags + ' -fuse-ld=lld -nostdlib++ -lc++_shared -Wl,--no-undefined -Wl,--exclude-libs,ALL')}
  toolchain_args = {{
    current_cpu = "arm64"
    current_os = "linux"
    is_clang = false
    use_remoteexec = false
  }}
}}
''')
    output = source / "out/ohos-arm64"
    output.mkdir(parents=True, exist_ok=True)
    (output / "args.gn").write_text('''target_os = "linux"
target_cpu = "arm64"
custom_toolchain = "//build/toolchain/feihong:ohos"
is_clang = false
is_debug = false
is_component_build = false
pdf_is_standalone = false
pdf_enable_v8 = false
pdf_enable_xfa = false
pdf_use_skia = false
pdf_use_partition_alloc = false
pdf_enable_fontations = false
pdf_enable_rust_png = false
icu_use_data_file = false
enable_rust = false
use_sysroot = false
use_custom_libcxx = true
use_custom_libcxx_for_host = false
use_llvm_libatomic = false
use_glib = false
use_siso = false
use_lld = false
clang_use_chrome_plugins = false
treat_warnings_as_errors = false
fatal_linker_warnings = false
symbol_level = 0
''')
    return output


def restore_build_patches(source):
    marker = source / ".feihong-patches.json"
    if not marker.is_file():
        return
    patches = json.loads(marker.read_text())
    for name, checksum in patches.items():
        if name not in PATCHED_PATHS:
            raise ValueError("Invalid generated patch marker")
        if hashlib.sha256((source / name).read_bytes()).hexdigest() != checksum:
            raise ValueError(f"Source was edited after build patches: {name}; use a separate work directory")
    # Each DEPS checkout is its own repository. Restore only generated files
    # whose exact content we verified, preserving unrelated local edits.
    repositories = [("build", BUILD_REVISION), ("buildtools", BUILDTOOLS_REVISION), ("", PDFIUM_REVISION)]
    remaining = set(patches)
    for directory, revision in repositories:
        names = [name for name in remaining if not directory or name.startswith(directory + "/")]
        if names:
            relative = [name[len(directory) + 1:] if directory else name for name in names]
            run(["git", "restore", "--source", revision, "--", *relative], source / directory)
            remaining.difference_update(names)
    marker.unlink()


def is_license_text(path):
    # depot_tools also contains license.py/ license_file.py implementation
    # files and LICENSE_OWNERS metadata. These are not authorization notices.
    name = path.name.upper()
    base = name.split(".", 1)[0]
    license_names = {"LICENSE", "COPYING", "LICENSE-MIT", "LICENSE-APACHE",
                     "LICENSE-BOOST", "LICENSE-BSD", "LICENSE-GPL", "LICENSE-LGPL"}
    text_suffixes = {".txt", ".md", ".rst", ".html", ".chromium"}
    return base in license_names and all(suffix.lower() in text_suffixes for suffix in path.suffixes)


def stage_licenses(source, licenses):
    licenses.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source / "LICENSE", licenses / "LICENSE")
    # Remove stale generated non-notices from earlier builds using the broader
    # LICENSE* prefix, so updating in place does not keep packaging tool code.
    for path in (licenses / "third_party").rglob("*"):
        if path.is_file() and path.name.upper().startswith(("LICENSE", "COPYING")) and not is_license_text(path):
            path.unlink()
    for path in (source / "third_party").rglob("*"):
        if path.is_file() and is_license_text(path) and ".git" not in path.parts:
            relative = path.relative_to(source)
            target = licenses / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(path, target)
    for relative in EXPLICIT_LICENSE_FILES:
        original = source / relative
        if not original.is_file():
            raise ValueError(f"Pinned PDFium license text missing: {relative}")
        target = licenses / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(original, target)
    return sorted(path.relative_to(licenses).as_posix() for path in licenses.rglob("*")
                  if path.is_file() and path.name != "build.json")


def stage(source, output, destination, licenses, sdk, cxx):
    candidates = [output / "libpdfium.so", output / "lib.unstripped/libpdfium.so"]
    library = next((p for p in candidates if p.is_file()), None)
    if library is None:
        raise ValueError("PDFium build did not produce libpdfium.so")
    readelf = sdk / "llvm/bin/llvm-readelf"
    header = run([readelf, "-h", library], capture=True)
    needed = run([readelf, "-d", library], capture=True)
    symbols = run([readelf, "--dyn-syms", library], capture=True)
    if "AArch64" not in header:
        raise ValueError("PDFium artifact is not AArch64")
    for bad in ["liblog.so", "libandroid.so", "libc.so.6", "libstdc++.so"]:
        if f"[{bad}]" in needed:
            raise ValueError(f"Foreign ABI dependency in PDFium: {bad}")
    for name in ["FPDF_InitLibraryWithConfig", "FPDF_LoadMemDocument64", "FPDF_RenderPageBitmap", "FPDF_GetPageCount", "FPDFText_LoadPage"]:
        if name not in symbols:
            raise ValueError(f"PDFium C ABI export missing: {name}")
    destination.mkdir(parents=True, exist_ok=True)
    shutil.copy2(library, destination / "libpdfium.so")
    license_files = stage_licenses(source, licenses)
    (licenses / "build.json").write_text(json.dumps({
        "release": "chromium/7881", "revision": PDFIUM_REVISION,
        "source": "https://pdfium.googlesource.com/pdfium.git",
        "target": "aarch64-linux-ohos", "sdk": str(sdk),
        "cxx": run([cxx, "--version"], capture=True),
        "libcxx_revision": LIBCXX_REVISION,
        "libcxxabi_revision": LIBCXXABI_REVISION,
        "build_revision": BUILD_REVISION, "buildtools_revision": BUILDTOOLS_REVISION,
        "sha256": hashlib.sha256(library.read_bytes()).hexdigest(),
        "license_files": license_files,
        "args_gn": (output / "args.gn").read_text(),
        "dynamic_dependencies": needed,
    }, indent=2) + "\n")


def build_probe(source, output, sdk):
    # This executable stays in the build output, never in the application HAP.
    run([sdk / "llvm/bin/clang", "--target=aarch64-linux-ohos", f"--sysroot={sdk / 'sysroot'}",
         "-std=c11", "-Wall", "-Wextra", "-DFEIHONG_PDFIUM_PROBE_STANDALONE=1", "-I", source,
         ROOT / "platform/ohos/tests/pdfium_probe.c", "-L", output,
         "-Wl,-rpath,$ORIGIN", f"-Wl,-rpath-link,{sdk / 'llvm/lib/aarch64-linux-ohos'}",
         "-lpdfium", "-o", output / "feihong_pdfium_probe"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--sdk", type=pathlib.Path, default=pathlib.Path("/Applications/DevEco-Studio.app/Contents/sdk/default/openharmony/native"))
    parser.add_argument("--work-dir", type=pathlib.Path, default=ROOT / "target/pdfium-worktree")
    parser.add_argument("--depot-tools", type=pathlib.Path, default=ROOT / "target/pdfium-depot-tools")
    parser.add_argument("--gn", type=pathlib.Path)
    parser.add_argument("--cxx", type=pathlib.Path, help="Clang 21+ compiler; defaults to the existing host clang++")
    parser.add_argument("--destination", type=pathlib.Path, default=ROOT / "platform/ohos/entry/libs/arm64-v8a")
    parser.add_argument("--licenses-directory", type=pathlib.Path, default=ROOT / "platform/ohos/entry/src/main/resources/rawfile/pdfium-licenses")
    parser.add_argument("--jobs", type=int, default=4)
    parser.add_argument("--skip-sync", action="store_true", help="Use an already synced pinned checkout")
    args = parser.parse_args()
    sdk, work, depot = args.sdk.resolve(), args.work_dir.resolve(), args.depot_tools.resolve()
    if not (sdk / "llvm/bin/clang++").is_file():
        parser.error("HarmonyOS NDK clang++ is unavailable")
    cxx = (args.cxx or pathlib.Path(shutil.which("clang++") or "clang++")).resolve()
    version = run([cxx, "--version"], capture=True)
    major = re.search(r"clang version (\d+)", version)
    if not major or int(major.group(1)) < 21:
        parser.error("PDFium 7881 requires --cxx pointing to Clang 21 or newer")
    work.mkdir(parents=True, exist_ok=True)
    source = work / "pdfium"
    if not args.skip_sync:
        checkout(depot, "https://chromium.googlesource.com/chromium/tools/depot_tools.git", DEPOT_REVISION)
        checkout(source, "https://pdfium.googlesource.com/pdfium.git", PDFIUM_REVISION)
        restore_build_patches(source)
        (work / ".gclient").write_text("solutions = " + repr([{
            "name": "pdfium", "url": "https://pdfium.googlesource.com/pdfium.git", "managed": False,
            "deps_file": "DEPS", "custom_vars": {"checkout_configuration": "minimal"},
            "custom_deps": {"pdfium/" + name: None for name in SKIP_DEPS},
        }]) + "\n")
        run([sys.executable, depot / "gclient.py", "sync", "--revision", "pdfium@" + PDFIUM_REVISION,
             "--no-history", "--shallow", "--nohooks", "--jobs", args.jobs,
             "--ignore-dep-type", "gcs", "--ignore-dep-type", "cipd"], work)
        # 7881's GN testing graph references simdutf even in a non-test build,
        # while DEPS's minimal checkout only enables it with V8. Fetch its
        # exact DEPS revision without enabling or downloading V8.
        checkout(source / "third_party/simdutf",
                 "https://chromium.googlesource.com/chromium/src/third_party/simdutf",
                 SIMDUTF_REVISION)
    if run(["git", "rev-parse", "HEAD"], source, True).strip() != PDFIUM_REVISION:
        raise ValueError("PDFium source revision must match the pinned version")
    for name, revision in [("libc++", LIBCXX_REVISION), ("libc++abi", LIBCXXABI_REVISION)]:
        if run(["git", "rev-parse", "HEAD"], source / "third_party" / name / "src", True).strip() != revision:
            raise ValueError(f"{name} source must match the pinned version")
    for name, revision in [("build", BUILD_REVISION), ("buildtools", BUILDTOOLS_REVISION)]:
        if run(["git", "rev-parse", "HEAD"], source / name, True).strip() != revision:
            raise ValueError(f"{name} source must match the pinned version")
    output = configure(source, sdk, cxx)
    gn = args.gn or work / "gn/gn"
    if not gn.is_file() and not args.skip_sync and not args.gn:
        host = "mac" if platform.system() == "Darwin" else "linux"
        arch = "arm64" if platform.machine() in ["arm64", "aarch64"] else "amd64"
        run([depot / "cipd", "install", f"gn/gn/{host}-{arch}", "git_revision:" + GN_REVISION,
             "-root", work / "gn"])
    if not gn.is_file():
        parser.error("Pinned GN binary missing; provide --gn or complete the gclient sync")
    run([gn.resolve(), "gen", output.relative_to(source)], source)
    run([shutil.which("ninja") or "ninja", "-C", output, "-j", args.jobs, "pdfium"], source)
    stage(source, output, args.destination.resolve(), args.licenses_directory.resolve(), sdk, cxx)
    build_probe(source, output, sdk)


if __name__ == "__main__":
    main()
