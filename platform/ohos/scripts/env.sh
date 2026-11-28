# Environment for building the OHOS platform bits natively on this machine.
#
# Usage:
#   . platform/ohos/scripts/env.sh
#   cd platform/ohos && cargo build --release -p slint-ohos-demo
#
# NOTE: the login environment already exports `OHOS_SDK` (pointing at an SDK
# *native* subdirectory), so this script deliberately uses OHOS_SDK_ROOT for
# the full SDK root and OHOS_SDK_NATIVE for the native SDK.
#
# The lld from the OHOS SDK needs the SDK's own libxml2: the harmonybrew copy
# lacks the symbols lld imports, so it must not shadow the SDK one.

OHOS_SDK_ROOT="${OHOS_SDK_ROOT:-$HOME/.harmonybrew/opt/ohos-sdk}"
OHOS_SDK_ROOT="${OHOS_SDK_ROOT%/}"
OHOS_SDK_NATIVE="${OHOS_SDK_NATIVE:-$OHOS_SDK_ROOT/native}"
export OHOS_SDK_ROOT OHOS_SDK_NATIVE

# The login environment may still name an older SDK package in these legacy
# variables. Keep them on the same package as OHOS_SDK_ROOT so tools that read
# them directly cannot mix headers or sysroots across an SDK upgrade.
export OHOS_SDK="$OHOS_SDK_ROOT/native"
export OHOS_SYSROOT="$OHOS_SDK_ROOT/native/sysroot"
export OHOS_CXX_INC="$OHOS_SDK_ROOT/native/llvm/include/libcxx-ohos/include/c++/v1"

# HarmonyOS SDK from DevEco Studio / Command Line Tools. Its layout differs
# from the OpenHarmony SDK above: sdk/default/{openharmony,hms}. The hms tree
# is where Scan Kit lives, so the HarmonyOS build needs this instead of the
# flat OpenHarmony SDK. Set HARMONY_CLT_ROOT when yours is elsewhere.
HARMONY_CLT_ROOT="${HARMONY_CLT_ROOT:-}"
if [ -z "$HARMONY_CLT_ROOT" ]; then
    for candidate in /data/service/hnp/hmos-clt.org/*; do
        if [ -f "$candidate/sdk/default/hms/ets/kits/@kit.ScanKit.d.ts" ]; then
            HARMONY_CLT_ROOT="$candidate"
            break
        fi
    done
fi
HARMONY_CLT_ROOT="${HARMONY_CLT_ROOT%/}"
if [ -n "$HARMONY_CLT_ROOT" ]; then
    HARMONY_SDK_ROOT="${HARMONY_SDK_ROOT:-$HARMONY_CLT_ROOT/sdk}"
    HARMONY_NODE_HOME="${HARMONY_NODE_HOME:-$HARMONY_CLT_ROOT/node/bin}"
    HARMONY_HVIGOR="${HARMONY_HVIGOR:-$HARMONY_CLT_ROOT/hvigor/bin/hvigorw}"
    HARMONY_RESTOOL="${HARMONY_RESTOOL:-$HARMONY_SDK_ROOT/default/openharmony/toolchains/restool}"
    HARMONY_HAP_SIGN_TOOL="${HARMONY_HAP_SIGN_TOOL:-$HARMONY_SDK_ROOT/default/openharmony/toolchains/lib/hap-sign-tool}"
    HARMONY_LLVM_STRIP="${HARMONY_LLVM_STRIP:-$HARMONY_SDK_ROOT/default/openharmony/native/llvm/bin/llvm-strip}"
    export HARMONY_SDK_ROOT HARMONY_NODE_HOME HARMONY_HVIGOR HARMONY_RESTOOL \
        HARMONY_HAP_SIGN_TOOL HARMONY_LLVM_STRIP
fi
export HARMONY_CLT_ROOT

case ":$LD_LIBRARY_PATH:" in
    *":$OHOS_SDK_ROOT/native/llvm/lib:"*) ;;
    *) LD_LIBRARY_PATH="$OHOS_SDK_ROOT/native/llvm/lib:$LD_LIBRARY_PATH" ;;
esac
export LD_LIBRARY_PATH

export HDC="${HDC:-$OHOS_SDK_ROOT/toolchains/hdc}"
export HAP_SIGN_TOOL="${HAP_SIGN_TOOL:-$OHOS_SDK_ROOT/toolchains/lib/hap-sign-tool}"
export HDC_TARGET="${HDC_TARGET:-127.0.0.1:36823}"

# hdc, hap-sign-tool etc. convenience PATH (optional).
case ":$PATH:" in
    *":$OHOS_SDK_ROOT/toolchains:"*) ;;
    *) PATH="$OHOS_SDK_ROOT/toolchains:$PATH" ;;
esac
export PATH
