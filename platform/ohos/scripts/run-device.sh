#!/bin/sh
# Install the demo hap on the local device, start it, and stream filtered logs.
#
# Usage: run-device.sh [hap]
#   hap defaults to platform/ohos/build/slint_ohos_demo-signed.hap
set -e

HERE=$(cd "$(dirname "$0")" && pwd)
PLATFORM_DIR=$(dirname "$HERE")
# shellcheck disable=SC1091
. "$HERE/env.sh"

HAP="${1:-$PLATFORM_DIR/build/slint_ohos_demo-signed.hap}"
BUNDLE="${BUNDLE:-com.flectar.mail_ohos}"

[ -f "$HAP" ] || { echo "[run-device] hap not found: $HAP (run build-hap.sh first)" >&2; exit 1; }

echo "[run-device] target: $HDC_TARGET"
echo "[run-device] installing: $HAP"
# `hdc install` refuses the local debug hap with "install version downgrade"
# (the shared bundle name carries the DevEco project's versionCode 1000000);
# bm install -r -d accepts the replacement. The hap must be on the device.
REMOTE="/data/local/tmp/$(basename "$HAP")"
"$HDC" -t "$HDC_TARGET" file send "$HAP" "$REMOTE"
"$HDC" -t "$HDC_TARGET" shell "bm install -r -d -p $REMOTE"

echo "[run-device] starting $BUNDLE/EntryAbility"
"$HDC" -t "$HDC_TARGET" shell aa start -a EntryAbility -b "$BUNDLE"

echo "[run-device] streaming hilog (Ctrl-C to stop)"
"$HDC" -t "$HDC_TARGET" shell hilog | grep -iE "slint|flectar|EntryAbility|panic|Rust"
