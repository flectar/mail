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

if [ -f "$TARGET" ]; then
    echo "pdfium already present: $TARGET"
    exit 0
fi

mkdir -p "$PREBUILT/arm64"
TMP="$PREBUILT/pdfium-7947-ohos-prebuilt.tar.gz"
if [ ! -f "$TMP" ]; then
    echo "downloading: $URL"
    curl -L --retry 3 -o "$TMP" "$URL"
fi
tar -xzf "$TMP" -C "$PREBUILT"
mv "$PREBUILT/pdfium-7947-ohos-prebuilt/lib/libpdfium.so" "$TARGET"
rm -rf "$PREBUILT/pdfium-7947-ohos-prebuilt"
echo "installed: $TARGET ($(stat -c %s "$TARGET") bytes)"
