#!/bin/sh
# Build an OHOS hap for this repo: Rust cdylib -> ArkTS shell abc -> staging -> sign.
#
# APP=demo (default) builds the Slint backend demo; APP=mail builds the
# flectar-mail shell (both share the ArkTS project; the XComponent page
# template selects the native library).
#
# Signing modes:
#   default         DevEco auto-signing material under $HOME/Documents/ohos/config
#                   (bound to this machine's UDID) through the ohos-workload
#                   sign-huawei.sh helper. The encrypted store password is read
#                   at run time from the DevEco project that generated the
#                   material; it is never stored in this repository.
#   RELEASE_SIGN=1  AGC release material passed through the environment
#                   (RELEASE_P12, RELEASE_CERT, RELEASE_PROFILE, key alias and
#                   passwords) and applied by hap-sign-tool directly.
#   SKIP_SIGN=1     Stop at the unsigned hap (build-only CI and pull-request
#                   checks, which must never touch signing material).
set -e

HERE=$(cd "$(dirname "$0")" && pwd)
PLATFORM_DIR=$(dirname "$HERE")
REPO_DIR=$(cd "$PLATFORM_DIR/../.." && pwd)
# shellcheck disable=SC1091
. "$HERE/env.sh"

log() { printf '[build-hap] %s\n' "$*"; }
die() { printf '[build-hap] ERROR: %s\n' "$*" >&2; exit 1; }

SDK="$OHOS_SDK_ROOT"
SHELL_DIR="$PLATFORM_DIR/shell"
BUILD_DIR="$PLATFORM_DIR/build"
STAGE="$BUILD_DIR/hap-stage"
# The DevEco auto-signing material is bundle-bound: override BUNDLE (for
# example with the previous name) until the signing profile is regenerated.
BUNDLE="${BUNDLE:-com.flectar.mail_ohos}"

# APP selects which native module the shell loads (page template + cdylib).
APP="${APP:-demo}"
case "$APP" in
    demo) CRATE="slint-ohos-demo"; LIB="slint_ohos_demo" ;;
    mail) CRATE="flectar-mail-ohos"; LIB="flectar_mail_ohos" ;;
    *) die "APP must be demo or mail (got: $APP)" ;;
esac
UNSIGNED="$BUILD_DIR/$LIB-unsigned.hap"
SIGNED="$BUILD_DIR/$LIB-signed.hap"

NODE_BIN="$(command -v node || true)"
[ -n "$NODE_BIN" ] || NODE_BIN="$HOME/.harmonybrew/bin/node"
HVIGOR_JS="${HVIGOR_JS:-$HOME/arkts-build/node_modules/@ohos/hvigor/bin/hvigor.js}"
JAVA_HOME="${JAVA_HOME:-$HOME/.harmonybrew/opt/openjdk@17}"
DEVECO_PROJECT_PROFILE="$HOME/Documents/DevEcoStudioProjects/MyApplication/build-profile.json5"
SIGN_CFG="${SIGN_CFG:-$HOME/Documents/ohos/config}"
SIGN_HELPER="${SIGN_HELPER:-$HOME/springsources/ohos-workload/scripts/sign-huawei.sh}"
HVIGOR_LOG="$BUILD_DIR/hvigor.log"

[ -f "$HVIGOR_JS" ] || die "hvigor not found: $HVIGOR_JS"
[ -d "$JAVA_HOME" ] || die "JAVA_HOME not found: $JAVA_HOME"
if [ "${SKIP_SIGN:-0}" != "1" ] && [ "${RELEASE_SIGN:-0}" != "1" ]; then
    [ -f "$SIGN_HELPER" ] || die "sign helper not found: $SIGN_HELPER"
fi

mkdir -p "$BUILD_DIR"

# 0. hvigor prerequisites ----------------------------------------------------
# hvigor resolves SDK components through a version-nested root referenced from
# local.properties (`sdk.dir=<root>/<platform-version>/<component>`). Both are
# generated here because they are machine-specific.
SDK_ROOT="$BUILD_DIR/sdk"
PLATFORM_VERSION="26.0.0"
if [ ! -e "$SDK_ROOT/$PLATFORM_VERSION/ets" ]; then
    rm -rf "$SDK_ROOT"
    mkdir -p "$SDK_ROOT/$PLATFORM_VERSION"
    for component in ets js native previewer toolchains; do
        [ -e "$SDK/$component" ] && ln -s "$SDK/$component" "$SDK_ROOT/$PLATFORM_VERSION/$component"
    done
fi
printf 'sdk.dir=%s\nnodejs.dir=%s\n' "$SDK_ROOT" "$(dirname "$(dirname "$NODE_BIN")")" \
    > "$SHELL_DIR/local.properties"

# 1. Rust cdylib -------------------------------------------------------------
# PROFILE=debug builds faster for first bring-up; release is the default.
PROFILE="${PROFILE:-release}"
if [ "$PROFILE" = debug ]; then
    CARGO_PROFILE_ARG=""
    SO_DIR="debug"
else
    CARGO_PROFILE_ARG="--release"
    SO_DIR="release"
fi
log "cargo build $PROFILE -p $CRATE"
( cd "$PLATFORM_DIR" && CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-3}" cargo build $CARGO_PROFILE_ARG -p "$CRATE" )
SO="$REPO_DIR/target/$SO_DIR/lib$LIB.so"
[ -f "$SO" ] || die "cdylib not found: $SO"

# 2. ArkTS shell -> modules.abc ----------------------------------------------
# hvigor's own PackageHap step needs the packing tool jar the SDK mirror lacks;
# the ArkTS output below is what matters (same approach as the workload's
# build-arkts-shell.sh, which tolerates that failure).
# NODE_PATH lets the generated hvigorfile.ts resolve `@ohos/hvigor-ohos-plugin`
# from the shared hvigor install (this project is not nested under its
# node_modules directory).
# The XComponent page pins the `libraryname`; pick the template for this APP.
cp "$SHELL_DIR/page-templates/Index.$APP.ets" "$SHELL_DIR/entry/src/main/ets/pages/Index.ets"
# Drop the previous ArkTS build outputs: hvigor's compiled modules.abc must be
# produced by this run, or a failed compile would silently package the previous
# APP's abc (its XComponent libraryname would load the wrong native module).
rm -rf "$SHELL_DIR/entry/build"
# Align AppScope/app.json5 with the selected bundle for this build: hvigor
# generates the ArkTS BuildProfile from it, so a BUNDLE override must reach
# this file as well as the final module.json.
APP_JSON5="$SHELL_DIR/AppScope/app.json5"
APP_JSON5_BAK="$BUILD_DIR/app.json5.bak"
cp "$APP_JSON5" "$APP_JSON5_BAK"
restore_app_json5() { mv "$APP_JSON5_BAK" "$APP_JSON5" 2>/dev/null || true; }
trap restore_app_json5 EXIT INT TERM
sed -i "s/bundleName: '[^']*'/bundleName: '$BUNDLE'/" "$APP_JSON5"
log "hvigor assembleHap (compile step; PackageHap failure is tolerated)"
( cd "$SHELL_DIR" && \
  env -i PATH="$(dirname "$NODE_BIN"):$JAVA_HOME/bin:/usr/bin:/bin" HOME="$HOME" \
      NODE_PATH="$HOME/arkts-build/node_modules" \
      JAVA_HOME="$JAVA_HOME" \
      OHOS_BASE_SDK_HOME="$SDK" DEVECO_SDK_HOME="$SDK" \
      externalApiPaths="$SDK/ets/api:$SDK/ets/kits:$SDK/ets/arkts" \
      "$NODE_BIN" "$HVIGOR_JS" \
        assembleHap -m module -p module=entry@default -p product=default -p buildMode=debug \
        > "$HVIGOR_LOG" 2>&1 ) || true

ABC="$(find "$SHELL_DIR/entry/build" -name modules.abc -path '*loader_out*' 2>/dev/null | head -1)"
[ -n "$ABC" ] || die "modules.abc not produced (see $HVIGOR_LOG)"
if grep -q 'Failed :entry:default@CompileArkTS' "$HVIGOR_LOG"; then
    tail -20 "$HVIGOR_LOG" | sed 's/^/    /'
    die "ArkTS compilation failed (see $HVIGOR_LOG)"
fi
log "modules.abc: $ABC"

# 3. hap staging -------------------------------------------------------------
log "staging hap layout"
rm -rf "$STAGE"
mkdir -p "$STAGE/ets" "$STAGE/libs/arm64-v8a"
cp "$ABC" "$STAGE/ets/modules.abc"
cp "$SO" "$STAGE/libs/arm64-v8a/"

# Prebuilt native libraries (libpdfium.so, fetching: scripts/fetch-pdfium.sh).
# The hap signing (signCode 1) covers every libs/** entry, so they ride along.
if [ -d "$PLATFORM_DIR/prebuilt/arm64" ]; then
    for lib in "$PLATFORM_DIR"/prebuilt/arm64/*.so; do
        [ -e "$lib" ] && cp "$lib" "$STAGE/libs/arm64-v8a/"
    done
fi

MERGED_JSON="$(find "$SHELL_DIR/entry/build" -name module.json -path '*merge_profile*' 2>/dev/null | head -1)"
if [ -n "$MERGED_JSON" ]; then
    cp "$MERGED_JSON" "$STAGE/module.json"
else
    die "merged module.json not found under hvigor build output"
fi

# The module.json field injections happen after the resource compile below,
# because they need the compiler's resolved resource ids (id table output).


# hvigor's own CompileResource feeds restool a config carrying the
# `compression` option; that path emits a legacy "Restool" resources.index
# which this device's resource manager cannot load — every $string/$media
# reference (ability label, app icon, colors) silently resolves to nothing and
# the launcher shows the bundle name with a blank icon. Re-compiling from the
# same config without `compression` produces the "RestoolV2" index that
# DevEco-built haps carry and the device loads.
RES_CFG="$(find "$SHELL_DIR/entry/build" -name resConfig.json -path '*intermediates/res/*' 2>/dev/null | head -1)"
[ -n "$RES_CFG" ] || die "hvigor resConfig.json not found under the build output"
RES_OUT="$BUILD_DIR/res-out"
rm -rf "$RES_OUT"
mkdir -p "$RES_OUT"
python3 - "$RES_CFG" "$RES_OUT" <<'PY'
import json, sys
config = json.load(open(sys.argv[1]))
out = sys.argv[2]
config.pop("compression", None)
config["output"] = out
config["ResourceTable"] = [f"{out}/ResourceTable.h", f"{out}/ResourceTable.ts"]
# The index format follows the manifest's SDK coordinates: an OpenHarmony
# target makes restool emit the legacy format this device cannot load. Patch
# the manifest restool validates against to the device's HarmonyOS values.
manifest = json.load(open(config["configPath"]))
app = manifest["app"]
app["compileSdkVersion"] = "6.1.0.105"
app["compileSdkType"] = "HarmonyOS"
app["targetAPIVersion"] = 60100023
app["minAPIVersion"] = 60100023
app["apiReleaseType"] = "Release"
with open(f"{out}/module.json", "w") as f:
    json.dump(manifest, f)
config["configPath"] = f"{out}/module.json"
# `ids`/`definedIds` keep the paths hvigor generated: restool reads the
# existing id table there and writes it back.
with open(f"{out}/resConfig.json", "w") as f:
    json.dump(config, f)
PY
log "compiling resources (restool, V2 index)"
"$SDK/toolchains/restool" -l "$RES_OUT/resConfig.json" > "$BUILD_DIR/restool.log" 2>&1 || {
    tail -5 "$BUILD_DIR/restool.log" | sed 's/^/    /'
    die "restool failed (see $BUILD_DIR/restool.log)"
}
[ -f "$RES_OUT/resources.index" ] || die "restool produced no resources.index"
head -c 10 "$RES_OUT/resources.index" | grep -q "RestoolV2" \
    || die "restool produced a legacy resources.index (the device cannot load it)"
cp "$RES_OUT/resources.index" "$STAGE/resources.index"
rm -rf "$STAGE/resources"
cp -r "$RES_OUT/resources" "$STAGE/resources"

# 3c. module.json injections -------------------------------------------------
# The device loads the ability from `ets/modules.abc` only when the manifest
# selects the panda VM (`virtualMachine`); without it the loader falls back to
# a per-file `ets/entryability/EntryAbility.abc` probe that fails for
# modules.abc-only haps. hvigor does not inject it; the workload's own
# packaging does (templates/module.json.template).
# deviceTypes: 2in1/tablet/phone (not 'default') — the desktop otherwise runs
# the window in phone-compatibility mode at ~0.59x and fonts look tiny.
# requestPermissions: without ohos.permission.INTERNET every socket operation
# (loopback OAuth listener, IMAP/SMTP) fails with EPERM, which surfaces as
# "OAuth failed: io error: Operation not permitted (os error 1)".
# READ_PASTEBOARD is deliberately absent: this device's installer rejects it
# for normal apps (ACL-only, "grant request permissions failed"), so clipboard
# reads stay unavailable; clipboard writes need no permission.
# SDK coordinates: the resource compiler only emits the V2 `resources.index`
# the device can load for HarmonyOS targets; DevEco-built haps carry these
# exact values.
# Resolved ids: hvigor's PackageHap step (unavailable here; it needs the
# packing-tool jar) normally resolves `$string:`/`$media:` references into the
# numeric `labelId`/`iconId`/... fields. Without them the launcher falls back
# to the bundle name with a blank icon, and the runtime lookup of the app's own
# resources fails ("Invalid resource ID ... not found in loaded hap/hsp
# packages"). Inject the same fields from the compiler's id table.
IDS_JSON="$(find "$SHELL_DIR/entry/build" -path '*ids_map/id_defined.json' 2>/dev/null | head -1)"
[ -n "$IDS_JSON" ] || die "id_defined.json not found under the hvigor build output"
python3 - "$STAGE/module.json" "$IDS_JSON" "$BUNDLE" <<'PY'
import json, sys
path, ids_path, bundle_name = sys.argv[1], sys.argv[2], sys.argv[3]
with open(ids_path) as f:
    records = json.load(f)["record"]
ids = {f"{record['type']}:{record['name']}": int(record["id"], 16) for record in records}
with open(path) as f:
    manifest = json.load(f)
module = manifest["module"]
module["compileMode"] = "esmodule"
module["virtualMachine"] = "ark13.0.1.0"
module["deviceTypes"] = ["2in1", "tablet", "phone"]
module.setdefault("libIsolation", True)
permissions = module.setdefault("requestPermissions", [])
if not any(entry.get("name") == "ohos.permission.INTERNET" for entry in permissions):
    permissions.append({"name": "ohos.permission.INTERNET"})
app = manifest["app"]
app["bundleName"] = bundle_name
app["compileSdkVersion"] = "6.1.0.105"
app["compileSdkType"] = "HarmonyOS"
app["targetAPIVersion"] = 60100023
app["minAPIVersion"] = 60100023
app["apiReleaseType"] = "Release"

def put(target, field, key):
    if key in ids:
        target[field] = ids[key]

put(app, "labelId", "string:app_name")
put(app, "iconId", "media:app_icon")
put(module, "descriptionId", "string:module_desc")
for ability in module.get("abilities", []):
    put(ability, "descriptionId", "string:EntryAbility_desc")
    put(ability, "labelId", "string:EntryAbility_label")
    put(ability, "iconId", "media:app_icon")
    put(ability, "startWindowIconId", "media:app_icon")
    put(ability, "startWindowBackgroundId", "color:start_window_background")
with open(path, "w") as f:
    json.dump(manifest, f, indent="\t")
PY

# 4. zip into an unsigned hap -----------------------------------------------
# -D: no directory entries. BMS's GetNativeLibraryFileNames counts every
# `libs/<abi>/...` zip entry - including the `libs/<abi>/` directory itself -
# as a native library, and the install then fails signature enforcement when
# the SoInfo segment has one entry fewer than that count.
# resources.index and resources/** are STORED (like DevEco-packed haps): the
# device's resource manager reads the index straight out of the installed hap
# and cannot load it from a deflated entry — the app then has no resources at
# all (raw "$string:" labels, default launcher icon, 9001001 lookups).
log "packing unsigned hap"
rm -f "$UNSIGNED" "$SIGNED"
python3 - "$UNSIGNED" "$STAGE" <<'PY'
import os, sys, zipfile
out, stage = sys.argv[1], sys.argv[2]
with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as archive:
    for root, _dirs, files in os.walk(stage):
        for name in files:
            path = os.path.join(root, name)
            entry = os.path.relpath(path, stage)
            stored = entry == "resources.index" or entry.startswith("resources/")
            archive.write(
                path,
                entry,
                compress_type=zipfile.ZIP_STORED if stored else zipfile.ZIP_DEFLATED,
            )
PY
log "unsigned: $UNSIGNED ($(stat -c%s "$UNSIGNED") bytes)"

# 5. sign --------------------------------------------------------------------
# SKIP_SIGN=1 stops at the unsigned hap (build-only CI; pull request checks
# never touch signing material). Default: the DevEco debug material (bound to
# this machine's UDID) through the ohos-workload helper. RELEASE_SIGN=1
# switches to the AGC release material supplied through the environment:
#   RELEASE_P12 (keystore) RELEASE_CERT (app certificate) RELEASE_PROFILE (signed .p7b)
#   RELEASE_KEY_ALIAS RELEASE_KEY_PWD RELEASE_STORE_PWD
if [ "${SKIP_SIGN:-0}" = "1" ]; then
    log "SKIP_SIGN=1: keeping the unsigned hap"
    SIGNED="$UNSIGNED"
elif [ "${RELEASE_SIGN:-0}" = "1" ]; then
    : "${RELEASE_P12:?RELEASE_SIGN=1 needs RELEASE_P12 (AGC keystore .p12)}"
    : "${RELEASE_CERT:?RELEASE_SIGN=1 needs RELEASE_CERT (AGC app certificate)}"
    : "${RELEASE_PROFILE:?RELEASE_SIGN=1 needs RELEASE_PROFILE (signed .p7b profile)}"
    : "${RELEASE_KEY_ALIAS:?RELEASE_SIGN=1 needs RELEASE_KEY_ALIAS}"
    : "${RELEASE_KEY_PWD:?RELEASE_SIGN=1 needs RELEASE_KEY_PWD}"
    : "${RELEASE_STORE_PWD:?RELEASE_SIGN=1 needs RELEASE_STORE_PWD}"
    log "signing with the AGC release material (bundle $BUNDLE)"
    "$HAP_SIGN_TOOL" sign-app -keyAlias "$RELEASE_KEY_ALIAS" -signAlg SHA256withECDSA \
        -mode localSign -signCode 1 \
        -appCertFile "$RELEASE_CERT" -profileFile "$RELEASE_PROFILE" \
        -inFile "$UNSIGNED" -outFile "$SIGNED" \
        -keystoreFile "$RELEASE_P12" \
        -keyPwd "$RELEASE_KEY_PWD" -keystorePwd "$RELEASE_STORE_PWD" >/dev/null \
        || die "release signing failed"
    "$HAP_SIGN_TOOL" verify-app -inFile "$SIGNED" \
        -outCertChain "$BUILD_DIR/.verify-cert.cer" -outProfile "$BUILD_DIR/.verify-profile.p7b" \
        2>&1 | grep -q "verify-app success" || die "release signature verification failed"
    rm -f "$BUILD_DIR/.verify-cert.cer" "$BUILD_DIR/.verify-profile.p7b"
else
    ENC_PWD="$(sed -n 's/.*"storePassword"[ \t]*:[ \t]*"\([0-9A-Fa-f]\{32,\}\)".*/\1/p' "$DEVECO_PROJECT_PROFILE" | head -1)"
    [ -n "$ENC_PWD" ] || die "cannot read the encrypted storePassword from $DEVECO_PROJECT_PROFILE"
    log "signing with the Huawei debug material (bundle $BUNDLE)"
    OHOS_ENC_PWD="$ENC_PWD" sh "$SIGN_HELPER" "$UNSIGNED" "$SIGNED" "$SIGN_CFG" \
        || die "signing failed"
fi

log "hap: $SIGNED"
