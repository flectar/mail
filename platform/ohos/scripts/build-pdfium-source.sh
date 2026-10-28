#!/bin/sh
# Build the OpenHarmony PDFium shared library from a pinned source revision.
#
# This is the reproducible alternative to prebuilt/arm64/libpdfium.so. It
# requires:
#   - the OpenHarmony SDK (OHOS_SDK_ROOT; API native toolchain + sysroot),
#   - a host that can run Chromium's GN/Ninja tools (normally Linux x86_64),
#   - network access for the source clone on the first run.
#
# The source tree is cached outside the repository. The resulting library is
# staged into prebuilt/arm64 and stripped; build-hap.sh packages it, its
# notices, and the repository license texts.
set -e

HERE=$(cd "$(dirname "$0")" && pwd)
PLATFORM_DIR=$(dirname "$HERE")
REPO_DIR=$(cd "$PLATFORM_DIR/../.." && pwd)
# shellcheck disable=SC1091
. "$HERE/env.sh"

SOURCE_REPO="${OHOS_PDFIUM_SOURCE_REPO:-https://gitcode.com/OpenHarmonyPCDeveloper/pdfium.git}"
SOURCE_REVISION="${OHOS_PDFIUM_REVISION:-f2a7573b5fd1071f61388aeb642329006ae22153}"
SOURCE_DIR="${OHOS_PDFIUM_SOURCE_DIR:-$HOME/.cache/flectar/ohos-pdfium}"
BUILD_DIR="${OHOS_PDFIUM_BUILD_DIR:-out/ohos_arm64}"
JOBS="${OHOS_PDFIUM_JOBS:-$(nproc 2>/dev/null || echo 4)}"
TARGET="$PLATFORM_DIR/prebuilt/arm64/libpdfium.so"

log() { printf '[build-pdfium] %s\n' "$*"; }
die() { printf '[build-pdfium] ERROR: %s\n' "$*" >&2; exit 1; }

[ -n "${OHOS_SDK_ROOT:-}" ] || die "OHOS_SDK_ROOT is not set"
[ -d "$OHOS_SDK_ROOT/native/llvm" ] || die "OHOS native LLVM toolchain not found under $OHOS_SDK_ROOT"
[ -d "$OHOS_SDK_ROOT/native/sysroot" ] || die "OHOS sysroot not found under $OHOS_SDK_ROOT"

command -v git >/dev/null 2>&1 || die "git is required"
if [ -n "${OHOS_PDFIUM_NINJA:-}" ]; then
    NINJA="$OHOS_PDFIUM_NINJA"
elif command -v ninja >/dev/null 2>&1; then
    NINJA=$(command -v ninja)
else
    NINJA=""
fi

if [ ! -d "$SOURCE_DIR/.git" ]; then
    log "cloning $SOURCE_REPO at $SOURCE_REVISION"
    mkdir -p "$(dirname "$SOURCE_DIR")"
    git clone --filter=blob:none "$SOURCE_REPO" "$SOURCE_DIR"
fi

cd "$SOURCE_DIR"
if ! git cat-file -e "$SOURCE_REVISION^{commit}" 2>/dev/null; then
    log "fetching pinned revision $SOURCE_REVISION"
    git fetch --depth 1 origin "$SOURCE_REVISION"
fi
git checkout --detach "$SOURCE_REVISION" >/dev/null
[ "$(git rev-parse HEAD)" = "$SOURCE_REVISION" ] || die "source checkout is not at the pinned revision"

# The OHOS port's GN configuration hard-codes //ohos_sdk/18/native/*. Point
# that path at the SDK selected by env.sh instead of downloading the older
# API-18 SDK from the source project's .install helper.
mkdir -p ohos_sdk
if [ -L ohos_sdk/18 ] || [ -e ohos_sdk/18 ]; then
    rm -rf ohos_sdk/18
fi
ln -s "$OHOS_SDK_ROOT" ohos_sdk/18

GN="${OHOS_PDFIUM_GN:-$SOURCE_DIR/third_party/depot_tools/gn}"
NINJA="${NINJA:-$SOURCE_DIR/third_party/depot_tools/ninja}"
[ -x "$GN" ] || die "GN not executable: $GN (set OHOS_PDFIUM_GN)"
[ -x "$NINJA" ] || die "Ninja not executable: $NINJA (set OHOS_PDFIUM_NINJA)"
if [ -z "${OHOS_PDFIUM_GN:-}" ] && [ "$(uname -m)" != "x86_64" ]; then
    die "the bundled GN is a linux64/x86_64 binary; run this on an x86_64 Linux host or set OHOS_PDFIUM_GN to an executable GN for this host"
fi

# Keep these arguments aligned with the OHOS PDFium project's build.sh. The
# source port currently supports API 18; newer SDK headers are used through
# the symlink above, with the sysroot from the same SDK.
GN_ARGS='target_os="ohos" target_cpu="arm64" is_debug=false is_official_build=false is_component_build=true use_sysroot=false use_custom_libcxx=false clang_use_chrome_plugins=false use_ozone=false build_chromium_with_ohos_src=false symbol_level=0'

log "gn gen $BUILD_DIR"
"$GN" gen "$BUILD_DIR" --args="$GN_ARGS"
log "ninja pdfium ($JOBS jobs)"
"$NINJA" -C "$BUILD_DIR" -j"$JOBS" pdfium

BUILT=$(find "$BUILD_DIR" -type f -name libpdfium.so | head -n 1)
COMPONENT_BUILD=false
if [ -z "$BUILT" ]; then
    # The OHOS port's default GN args use is_component_build=true. PDFium is
    # then emitted as libpdfium.cr.so with its sibling .cr.so dependencies;
    # package those together under the stable libpdfium.so name.
    BUILT=$(find "$BUILD_DIR" -type f -name libpdfium.cr.so | head -n 1)
    COMPONENT_BUILD=true
fi
[ -n "$BUILT" ] || die "libpdfium.so/libpdfium.cr.so was not produced"

mkdir -p "$PLATFORM_DIR/prebuilt/arm64"
cp "$BUILT" "$TARGET"
if [ "$COMPONENT_BUILD" = true ]; then
    find "$BUILD_DIR" -type f -name '*.cr.so' -exec cp -f {} "$PLATFORM_DIR/prebuilt/arm64/" \;
    # The component build links the C++ runtime dynamically. The OHOS app
    # namespace does not search /system/lib64 for this, so bundle the matching
    # SDK copy beside libpdfium.so.
    CXX_SHARED="$OHOS_SDK_ROOT/native/llvm/lib/aarch64-linux-ohos/libc++_shared.so"
    [ -f "$CXX_SHARED" ] || die "component build needs $CXX_SHARED"
    cp "$CXX_SHARED" "$PLATFORM_DIR/prebuilt/arm64/libc++_shared.so"
    log "component build: bundled C++ runtime and $(find "$PLATFORM_DIR/prebuilt/arm64" -maxdepth 1 -name '*.cr.so' | wc -l) dependency libraries"
fi
STRIP="$OHOS_SDK_ROOT/native/llvm/bin/llvm-strip"
if [ -x "$STRIP" ]; then
    for lib in "$PLATFORM_DIR"/prebuilt/arm64/*.so; do
        [ -e "$lib" ] && "$STRIP" --strip-unneeded "$lib"
    done
fi
cp "$SOURCE_DIR/LICENSE" "$PLATFORM_DIR/prebuilt/PDFium-LICENSE"
log "installed: $TARGET ($(stat -c%s "$TARGET") bytes)"
log "license: $PLATFORM_DIR/prebuilt/PDFium-LICENSE"
