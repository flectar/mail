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
OHOS_SDK_NATIVE="${OHOS_SDK_NATIVE:-$HOME/.harmonybrew/opt/ohos-sdk-native}"
export OHOS_SDK_ROOT OHOS_SDK_NATIVE

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
