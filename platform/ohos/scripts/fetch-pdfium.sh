#!/bin/sh
# Fetch the OpenHarmony PDFium prebuilt (aarch64-linux-ohos) used by the OHOS
# mail shell. The binaries are not stored in the repository; run this once per
# checkout (build-hap.sh stages prebuilt/arm64/*.so into the hap when present).
#
# Source: https://gitcode.com/OpenHarmonyPCDeveloper/ohos_pdfium (release 7947,
# matching pypdfium2 5.12.1). The pdfium-render crate in the app binds at
# runtime with the `pdfium_7881` API set; 7947 carries the same public API.
set -e

HERE=$(cd "$(dirname "$0")" && pwd)
PLATFORM_DIR=$(dirname "$HERE")
PREBUILT="$PLATFORM_DIR/prebuilt"
TARGET="$PREBUILT/arm64/libpdfium.so"
URL="https://gitcode.com/OpenHarmonyPCDeveloper/ohos_pdfium/releases/download/7947/pdfium-7947-ohos-prebuilt.tar.gz"
# Pinned archive checksum: the prebuilt runs inside the app, so never extract
# an unverified download.
EXPECTED_SHA256="85126a7d9ddd1be6b5cc8ff84e9027777f36345ce8dcae1c6cd58262d6ae4fd5"

sha256_of() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

if [ -f "$TARGET" ]; then
    echo "pdfium already present: $TARGET"
    exit 0
fi

mkdir -p "$PREBUILT/arm64"
TMP="$PREBUILT/pdfium-7947-ohos-prebuilt.tar.gz"
if [ ! -f "$TMP" ]; then
    echo "downloading: $URL"
    curl -fL --retry 3 -o "$TMP" "$URL"
fi
actual=$(sha256_of "$TMP")
if [ "$actual" != "$EXPECTED_SHA256" ]; then
    echo "checksum mismatch for $TMP" >&2
    echo "  expected: $EXPECTED_SHA256" >&2
    echo "  actual:   $actual" >&2
    rm -f "$TMP"
    exit 1
fi
tar -xzf "$TMP" -C "$PREBUILT"
mv "$PREBUILT/pdfium-7947-ohos-prebuilt/lib/libpdfium.so" "$TARGET"
rm -rf "$PREBUILT/pdfium-7947-ohos-prebuilt"
echo "installed: $TARGET ($(stat -c %s "$TARGET") bytes)"
